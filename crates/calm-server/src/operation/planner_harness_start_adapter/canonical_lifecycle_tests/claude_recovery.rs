use super::*;

#[tokio::test]
async fn canonical_claude_recovery_validated_binding_reaches_provider_readiness() {
    let fx = Fixture::with_provider(false, AgentProvider::Claude).await;
    let (_auth, _token, source) = source().await;
    let error = wake(&fx, source).await.unwrap_err();
    assert!(
        error.to_string().contains("--claude-planner-config"),
        "{error}"
    );
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(row.thread_id.is_none());
    assert!(row.agent_session_id.is_none());
    assert!(row.spawn_op_id.is_none());
    assert!(row.mcp_token_hash.is_none());
}

impl Fixture {
    async fn claude() -> Self {
        let mut fx = Self::with_provider(false, AgentProvider::Claude).await;
        let bin = fx._dir.path().join("claude");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/claude_planner_fake/claude.sh"
            ),
            &bin,
        )
        .unwrap();
        std::fs::write(fx._dir.path().join("scenario"), "stream").unwrap();
        std::fs::write(fx._dir.path().join("stream"), "").unwrap();
        std::fs::write(fx._dir.path().join("stream-rest"), "").unwrap();
        fx.state = fx.state.with_claude_planner_config(
            crate::claude_planner::config::ClaudePlannerConfig {
                claude_binary: bin,
                claude_version: "2.1.280".into(),
                config_dir: fx._dir.path().join("config"),
            },
        );
        fx
    }

    async fn row(&self) -> calm_types::worker::WorkerSession {
        self.repo
            .session_get(&calm_types::worker::WorkerSessionId(self.worker.clone()))
            .await
            .unwrap()
            .unwrap()
    }

    async fn frame(&self) -> CanonicalHarnessSnapshot {
        serde_json::from_value(self.row().await.handle_state_json.unwrap()).unwrap()
    }

    async fn attempted(&self) -> CanonicalHarnessSnapshot {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let frame = self.frame().await;
                if frame
                    .canonical_inputs
                    .attempts
                    .first()
                    .is_some_and(|attempt| {
                        matches!(
                            attempt.delivery,
                            calm_types::conversation::InputDeliveryFact::Unknown
                        )
                    })
                {
                    return frame;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    fn lines(&self) -> usize {
        std::fs::read_to_string(self._dir.path().join("stdin"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    async fn wait_for_lines(&self, minimum: usize) -> usize {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let lines = self.lines();
                if lines >= minimum {
                    return lines;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn bind_with_lost_response(&self) -> (String, String) {
        let (auth, token, source) = source().await;
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "retained-local-bound",
            self.card.as_str(),
            pause.clone(),
        );
        let activation = wake(self, source);
        let observe = async {
            tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
                .await
                .unwrap();
            let row = self.row().await;
            let thread = row.thread_id.clone().unwrap();
            assert!(uuid::Uuid::parse_str(&thread).is_ok());
            assert!(row.agent_session_id.is_none());
            assert!(row.mcp_token_hash.is_none());
            assert_eq!(
                self.frame().await.header.last_thread_id.as_deref(),
                Some(thread.as_str())
            );
            assert!(!self._dir.path().join("spawns").exists());
            assert_eq!(self.lines(), 0);
            auth.sessions.remove(&token).unwrap();
            pause.release.notify_one();
            (thread, row.spawn_op_id.unwrap())
        };
        let (result, identity) = tokio::join!(activation, observe);
        assert!(result.is_err());
        identity
    }
}

#[tokio::test]
async fn canonical_claude_recovery_activation_replay_binds_one_uuid_without_cli() {
    let fx = Fixture::claude().await;
    let (thread, op) = fx.bind_with_lost_response().await;
    let (_auth, _token, source) = source().await;
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-issuance-track-held",
        fx.card.as_str(),
        pause.clone(),
    );
    wake(&fx, source.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    // The original loop owns Track here, before its durable claim. Replay must
    // coexist with that issuance without minting or delivering another input.
    let waiting = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "key-lock-waiting",
        &fx.payload.track_id,
        waiting.clone(),
    );
    let replay = wake(&fx, source);
    tokio::pin!(replay);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            result = &mut replay => result,
            _ = waiting.entered.notified() => {
                pause.release.notify_one();
                waiting.release.notify_one();
                replay.await
            }
        }
    })
    .await;
    pause.release.notify_one();
    waiting.release.notify_one();
    result.unwrap().unwrap();
    let row = fx.row().await;
    assert_eq!(row.thread_id.as_deref(), Some(thread.as_str()));
    assert_eq!(row.spawn_op_id.as_deref(), Some(op.as_str()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operations")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        1
    );
    let frame = fx.attempted().await;
    assert_eq!(frame.canonical_inputs.attempts.len(), 1);
    assert_eq!(fx.wait_for_lines(1).await, 1);
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
}

#[tokio::test]
async fn canonical_claude_recovery_revocation_after_workspace_or_bind_has_zero_lines() {
    let fx = Fixture::claude().await;
    let (thread, _) = fx.bind_with_lost_response().await;
    assert_eq!(fx.row().await.thread_id.as_deref(), Some(thread.as_str()));
    assert_eq!(fx.lines(), 0);
    assert!(fx.state.harness.get(&fx.worker).is_none());
    let (auth, token, source) = source().await;
    auth.sessions.remove(&token).unwrap();
    assert!(wake(&fx, source).await.is_err());
    assert_eq!(fx.lines(), 0);
}

#[tokio::test]
async fn canonical_claude_recovery_crossing_and_unknown_never_write_again() {
    use calm_types::conversation::InputDeliveryFact;
    #[derive(Clone, Copy)]
    enum Case {
        BeforeInitUnknown,
        BeforeInitCrossing,
        ResumedUnknown,
    }
    for case in [
        Case::BeforeInitUnknown,
        Case::BeforeInitCrossing,
        Case::ResumedUnknown,
    ] {
        let fx = Fixture::claude().await;
        let (_auth, _token, source) = source().await;
        let (crossing, resumed) = match case {
            Case::BeforeInitUnknown => (false, false),
            Case::BeforeInitCrossing => (true, false),
            Case::ResumedUnknown => (false, true),
        };
        if resumed {
            std::fs::write(fx._dir.path().join("scenario"), "exit").unwrap();
            wake(&fx, source.clone()).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(15), async {
                loop {
                    let frame = fx.frame().await;
                    if frame.header.phase == HarnessPhaseTag::TurnCompleted
                        && frame.canonical_inputs.active.is_none()
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(fx.row().await.agent_session_id.is_some());
            fx.state
                .harness
                .shutdown_and_remove(&fx.worker)
                .await
                .unwrap();
            let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
            let mut carrier = crate::db::sqlite::conversation::current_input_carrier_tx(
                &mut tx,
                fx.card.as_str(),
                &fx.worker,
            )
            .await
            .unwrap();
            crate::conversation_inputs::accept_human_tx(
                &mut tx,
                &source.current_principal().unwrap(),
                &mut carrier,
                HumanPayload {
                    text: "second original input".into(),
                    bound_attachments: vec![],
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            std::fs::write(fx._dir.path().join("scenario"), "stream").unwrap();
        } else {
            let scenario = if crossing { "undecodable" } else { "stall" };
            std::fs::write(fx._dir.path().join("scenario"), scenario).unwrap();
        }
        if crossing {
            sqlx::query(
                "CREATE TRIGGER fail_fact BEFORE UPDATE OF handle_state_json ON worker_sessions
                WHEN json_extract(NEW.handle_state_json,
                    '$.canonical_inputs.attempts[0].delivery.kind')='unknown'
                BEGIN SELECT RAISE(ABORT,'fact failed'); END",
            )
            .execute(fx.repo.pool())
            .await
            .unwrap();
        }
        wake(&fx, source.clone()).await.unwrap();
        let expected_lines = match case {
            Case::BeforeInitUnknown => 0,
            Case::BeforeInitCrossing => 1,
            Case::ResumedUnknown => 2,
        };
        let expected_spawns = if resumed { 2 } else { 1 };
        let saved = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let frame = fx.frame().await;
                let held = frame.canonical_inputs.active.is_some()
                    && frame
                        .canonical_inputs
                        .attempts
                        .last()
                        .is_some_and(|attempt| {
                            if crossing {
                                matches!(attempt.delivery, InputDeliveryFact::CrossingUnknown)
                            } else {
                                matches!(attempt.delivery, InputDeliveryFact::Unknown)
                            }
                        });
                let spawns = std::fs::read_to_string(fx._dir.path().join("spawns"))
                    .unwrap_or_default()
                    .lines()
                    .count();
                if held && spawns == expected_spawns && fx.lines() == expected_lines {
                    break frame;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let native_before = fx.row().await.agent_session_id;
        if resumed {
            let lines = std::fs::read_to_string(fx._dir.path().join("stdin")).unwrap();
            assert_eq!(lines.matches("second original input").count(), 1);
            assert!(
                std::fs::read_to_string(fx._dir.path().join("argv"))
                    .unwrap()
                    .lines()
                    .any(|arg| arg == "--resume")
            );
            assert!(native_before.is_some());
        } else {
            assert!(native_before.is_none(), "no native init was observed");
        }
        fx.state
            .harness
            .shutdown_and_remove(&fx.worker)
            .await
            .unwrap();
        if crossing {
            sqlx::query("DROP TRIGGER fail_fact")
                .execute(fx.repo.pool())
                .await
                .unwrap();
        }
        // Lose only the END checkpoint, as at a host crash after process settlement.
        let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
        session_set_handle_state_tx(
            &mut tx,
            &fx.worker,
            Some(serde_json::to_value(&saved).unwrap()),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let reopened = SqlxRepo::open(&format!(
            "sqlite://{}",
            fx._dir.path().join("canonical.sqlite").display()
        ))
        .await
        .unwrap();
        let actual = reopened
            .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
            .await
            .unwrap()
            .unwrap();
        let frame: CanonicalHarnessSnapshot =
            serde_json::from_value(actual.handle_state_json.unwrap()).unwrap();
        assert_eq!(frame.canonical_inputs, saved.canonical_inputs);
        assert_eq!(actual.agent_session_id, native_before);
        wake(&fx, source).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        assert!(
            fx.state.harness.get(&fx.worker).is_some(),
            "held control must reopen"
        );
        assert_eq!(fx.frame().await.canonical_inputs, saved.canonical_inputs);
        assert_eq!(fx.row().await.agent_session_id, native_before);
        assert_eq!(fx.lines(), expected_lines);
        assert_eq!(
            std::fs::read_to_string(fx._dir.path().join("spawns"))
                .unwrap()
                .lines()
                .count(),
            expected_spawns
        );
        fx.state
            .harness
            .shutdown_and_remove(&fx.worker)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_recovery_crash_before_marker_requires_live_source() {
    let fx = Fixture::claude().await;
    let (thread, _) = fx.bind_with_lost_response().await;
    sqlx::query(
        "CREATE TRIGGER fail_marker BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN NEW.handle_state_json LIKE '%crossing_unknown%'
        BEGIN SELECT RAISE(ABORT,'marker failed'); END",
    )
    .execute(fx.repo.pool())
    .await
    .unwrap();
    let (auth, token, old_source) = source().await;
    wake(&fx, old_source.clone()).await.unwrap();
    let saved = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let frame = fx.frame().await;
            if frame.canonical_inputs.active.is_some() {
                break frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
    assert!(saved.canonical_inputs.attempts.is_empty());
    auth.sessions.remove(&token).unwrap();
    sqlx::query("DROP TRIGGER fail_marker")
        .execute(fx.repo.pool())
        .await
        .unwrap();
    assert!(wake(&fx, old_source).await.is_err());
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    assert_eq!(fx.lines(), 0);
    assert_eq!(fx.frame().await.canonical_inputs, saved.canonical_inputs);
    let (_auth, _token, live) = source().await;
    wake(&fx, live).await.unwrap();
    let running = fx.attempted().await;
    assert_eq!(
        running.header.last_thread_id.as_deref(),
        Some(thread.as_str())
    );
    assert_eq!(
        running.canonical_inputs.active,
        saved.canonical_inputs.active
    );
    assert_eq!(fx.wait_for_lines(1).await, 1);
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
}

#[tokio::test]
async fn canonical_claude_recovery_exact_predecessor_stop_failure_stays_held() {
    let fx = Fixture::claude().await;
    fx.bind_with_lost_response().await;
    let saved = fx.frame().await;
    let (_auth, _token, source) = source().await;
    let host = fx.state.planner_wiring().claude;
    let binary = fx._dir.path().join("claude");
    let original = std::fs::read(&binary).unwrap();
    std::fs::write(
        &binary,
        r#"#!/bin/sh
touch "$0.ready"
exec sleep 300
"#,
    )
    .unwrap();
    let thread = fx.row().await.thread_id.unwrap();
    let session = crate::claude_planner::session::ClaudePlannerSession::open(
        crate::claude_planner::session::ClaudePlannerSessionParams {
            engine: host.execution.engine().cloned(),
            host: host.clone(),
            worker_session_id: fx.worker.clone(),
            card_id: fx.card.to_string(),
            track_id: fx.payload.track_id.clone(),
            cwd: fx._dir.path().into(),
            instructions: "predecessor rewind".into(),
            calm_tools: host.calm_tools.clone(),
            proxy: vec![],
            prior_total_tokens: 0,
            repo: fx.repo.clone(),
            seals: Arc::new(crate::thread_seals::ThreadSeals::new()),
        },
    )
    .await
    .unwrap();
    let check = tokio::spawn(async move { session.prepare_edit_for_test(&thread).await });
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !fx._dir.path().join("claude.ready").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    crate::test_support::execution::block_stops(fx.repo.db()).await;
    check.abort();
    let _ = check.await;
    assert!(
        crate::claude_planner::recovery::quiesce_canonical(&host, &fx.worker)
            .await
            .is_err()
    );
    let result = wake(&fx, source.clone()).await;
    crate::test_support::execution::unblock_stops(fx.repo.db()).await;
    assert!(result.is_err());
    assert!(fx.state.harness.get(&fx.worker).is_none());
    assert_eq!(fx.frame().await, saved);
    assert_eq!(fx.lines(), 0);
    crate::claude_planner::recovery::quiesce_canonical(&host, &fx.worker)
        .await
        .unwrap();
    std::fs::write(&binary, original).unwrap();
    wake(&fx, source).await.unwrap();
    fx.attempted().await;
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
}

#[tokio::test]
async fn canonical_claude_recovery_workspace_rechecks_membership_owner_and_carrier() {
    enum Change {
        Logout,
        Membership,
        OperationOwner,
        Carrier,
    }
    for change in [
        Change::Logout,
        Change::Membership,
        Change::OperationOwner,
        Change::Carrier,
    ] {
        let fx = Fixture::claude().await;
        let (auth, token, live) = source().await;
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "retained-workspace-ready",
            fx.card.as_str(),
            pause.clone(),
        );
        let activation = wake(&fx, live);
        let invalidation = async {
            tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
                .await
                .unwrap();
            match change {
                Change::Logout => {
                    auth.sessions.remove(&token).unwrap();
                }
                Change::Membership => {
                    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
                    let area = crate::db::sqlite::conversation::conversation_area_tx(
                        &mut tx,
                        fx.card.as_str(),
                    )
                    .await
                    .unwrap();
                    crate::db::sqlite::area_membership::application_user_register(
                        &mut tx, "alice", "Alice",
                    )
                    .await
                    .unwrap();
                    sqlx::query(
                        "UPDATE area_members SET role='member' WHERE area_id=?1 AND role='owner'",
                    )
                    .bind(area.id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                    sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','owner',0)")
                        .bind(area.id.as_str()).execute(&mut **tx).await.unwrap();
                    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id=?2")
                        .bind(area.id.as_str())
                        .bind(LEGACY_OWNER_USER_ID)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
                Change::OperationOwner => {
                    sqlx::query("UPDATE operations SET lease_owner='replacement' WHERE phase='app_server_interact'")
                        .execute(fx.repo.pool()).await.unwrap();
                }
                Change::Carrier => {
                    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
                    session_fail_if_active_runtime_tx(&mut tx, &fx.worker)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
            }
            pause.release.notify_one();
        };
        let (result, ()) = tokio::join!(activation, invalidation);
        assert!(result.is_err());
        let row = fx.row().await;
        assert!(row.thread_id.is_none());
        assert!(row.agent_session_id.is_none());
        assert!(row.mcp_token_hash.is_none());
        assert!(row.spawn_op_id.is_none());
        assert_eq!(fx.lines(), 0);
        assert!(fx.state.harness.get(&fx.worker).is_none());
    }
}

#[tokio::test]
async fn canonical_claude_compact_route_requires_real_human_and_retained_recovery_wedges() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let fx = Fixture::claude().await;
    std::fs::write(fx._dir.path().join("scenario"), "exit").unwrap();
    let (_auth, _token, human) = source().await;
    wake(&fx, human.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while fx.frame().await.header.phase != HarnessPhaseTag::TurnCompleted {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(fx._dir.path().join("scenario"), "compact").unwrap();
    let app = axum::Router::new()
        .route(
            "/compact/{id}",
            axum::routing::post(crate::routes::planner_compact::compact_planner_card),
        )
        .with_state(RouteState::from_ref(&fx.state));
    let uri = format!("/compact/{}", fx.card);
    let response = app
        .clone()
        .oneshot(
            Request::post(&uri)
                .extension(crate::actor::Actor("user".into()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "declared actor grants no Human source"
    );
    let response = app
        .clone()
        .oneshot(
            Request::post(&uri)
                .extension(crate::actor::Actor("ai:codex".into()))
                .extension(human.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(fx.lines(), 1);
    let response = app
        .oneshot(
            Request::post(&uri)
                .extension(crate::actor::Actor("user".into()))
                .extension(human.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while fx
            .frame()
            .await
            .header
            .compact_outcome
            .as_ref()
            .is_none_or(|outcome| {
                outcome.status != crate::harness::compact_outcome::CompactStatus::Completed
            })
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
    let before = fx.frame().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = crate::db::sqlite::conversation::current_input_carrier_tx(
        &mut tx,
        fx.card.as_str(),
        &fx.worker,
    )
    .await
    .unwrap();
    let fence = crate::db::sqlite::conversation::RuntimeExecutionFence::from_carrier(&carrier);
    let mut header = fence.header().clone();
    header["phase"] = json!("compacting");
    header["compact_outcome"]["status"] = json!("started");
    crate::db::sqlite::conversation::persist_runtime_frame_tx(
        &mut tx,
        fx.card.as_str(),
        &fx.worker,
        &fence,
        header,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    wake(&fx, human).await.unwrap();
    let after = fx.frame().await;
    assert_eq!(after.header.phase, HarnessPhaseTag::Wedged);
    assert_eq!(
        after.header.compact_outcome.unwrap().status,
        crate::harness::compact_outcome::CompactStatus::Unconfirmed
    );
    assert_eq!(after.canonical_inputs, before.canonical_inputs);
    assert_eq!(after.header.last_turn_id, before.header.last_turn_id);
    assert_eq!(
        fx.lines(),
        2,
        "retained recovery never writes another compact line"
    );
    fx.state
        .harness
        .shutdown_and_remove(&fx.worker)
        .await
        .unwrap();
}
