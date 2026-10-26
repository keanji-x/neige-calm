use super::*;

async fn child_track_binding(boot: &Boot) -> String {
    let parent = boot
        .repo
        .track_create(NewTrack {
            area_id: boot.area_id.clone(),
            title: "parent".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let pool = boot.repo.sqlite_db().unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(parent.id.as_str())
        .bind(boot.track_id.as_str())
        .execute(pool.writer_pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,child_track_id, \
        created_at_ms,updated_at_ms) VALUES('assignment',?1,'assignment','codex','goal','{}','running','sub-track',?2,1,1)")
        .bind(parent.id.as_str()).bind(boot.track_id.as_str()).execute(pool.writer_pool()).await.unwrap();
    calm_server::db::write_with_event_typed(
        boot.repo.as_ref(),
        ActorId::KernelDispatcher,
        calm_server::event::EventScope::Track {
            track: parent.id.clone(),
            area: boot.area_id.clone(),
        },
        None,
        &boot.ctx.events,
        &boot.ctx.write,
        |_| {
            Box::pin(async {
                Ok((
                    (),
                    Event::TaskDispatched {
                        idempotency_key: "assignment".into(),
                        kind: "codex".into(),
                        agent_message: None,
                    },
                ))
            })
        },
    )
    .await
    .unwrap();
    boot.repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
        .await
        .unwrap();
    parent.id.to_string()
}

#[tokio::test]
async fn child_track_handback_fails_parent_leaves_child_open_and_fences_pending_claim() {
    let boot = boot().await;
    let parent_id = child_track_binding(&boot).await;
    insert_task(&boot, "left", "left", "pending", None).await;
    let report = boot
        .repo
        .cards_by_track(boot.track_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind == "track-report")
        .unwrap();
    let expected_candidate = crate::support::child_track::seed_candidates(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        report.id.as_str(),
    )
    .await;
    let mut rx = boot.ctx.events.subscribe();
    let result = call_tool(
        &boot,
        "neige_task_fail",
        planner_identity(&boot),
        json!({"attempt_id":"assignment","reason":"Need a decision"}),
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        json!({"source":"child-track","outcome":"handed_back","child_track_id":boot.track_id,
        "report":{"card_id":report.id,"rev":0},"candidate":expected_candidate,"blocked_on":"Need a decision"})
    );
    assert_eq!(
        boot.repo
            .task_get("assignment")
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Failed
    );
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_none()
    );
    let envelope = rx.recv().await.unwrap();
    assert_eq!(envelope.actor, ActorId::KernelDispatcher);
    assert_eq!(envelope.scope.track_id().unwrap().as_str(), parent_id);
    match envelope.event {
        Event::TaskFailed {
            details: Some(details),
            ..
        } => assert_eq!(details, result),
        event => panic!("expected handback failure, got {event:?}"),
    }
    let parent = boot.repo.track_get(&parent_id).await.unwrap().unwrap();
    let view = calm_truth::track_fs_view::TrackFsView::new(boot.repo.as_ref(), &boot.ctx.write);
    let record = view.cat(&parent, "runs/assignment.json").await.unwrap();
    let record: Value = serde_json::from_str(&record.content).unwrap();
    assert_eq!(record["events"]["failed"]["payload"]["details"], result);
    let db = boot.repo.sqlite_db().unwrap();
    let mut tx = db.writer_pool().begin().await.unwrap();
    assert_eq!(
        calm_truth::db::sqlite::task_claim_pending_tx(&mut tx, "left", 2, &[], false)
            .await
            .unwrap(),
        0
    );
    tx.rollback().await.unwrap();
    assert!(
        call_tool(
            &boot,
            "neige_task_fail",
            planner_identity(&boot),
            json!({"attempt_id":"assignment","reason":"again"})
        )
        .await
        .is_err()
    );
    assert!(
        rx.try_recv().is_err(),
        "duplicate handback must not emit an event"
    );
}

#[tokio::test]
async fn child_track_handback_refuses_inflight_without_partial_writes() {
    for status in ["dispatched", "running", "verifying"] {
        let boot = boot().await;
        child_track_binding(&boot).await;
        insert_task(&boot, "busy", "busy", status, None).await;
        let mut rx = boot.ctx.events.subscribe();
        let error = call_tool(
            &boot,
            "neige_task_fail",
            planner_identity(&boot),
            json!({"attempt_id":"assignment","reason":"Need a decision"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32409, "{error:?}");
        assert_eq!(
            boot.repo
                .task_get("assignment")
                .await
                .unwrap()
                .unwrap()
                .status,
            calm_server::model::TaskStatus::Running
        );
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn child_track_claim_requires_live_parent_binding() {
    let boot = boot().await;
    child_track_binding(&boot).await;
    let db = boot.repo.sqlite_db().unwrap();
    for (id, active) in [("before", true), ("after", false)] {
        insert_task(&boot, id, id, "pending", None).await;
        if !active {
            sqlx::query("UPDATE tasks SET status='failed' WHERE id='assignment'")
                .execute(db.writer_pool())
                .await
                .unwrap();
        }
        let mut tx = db.writer_pool().begin().await.unwrap();
        let changed = calm_truth::db::sqlite::task_claim_pending_tx(&mut tx, id, 2, &[], false)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(changed, u64::from(active), "claim {id}");
    }
}

#[tokio::test]
async fn child_track_reports_catalog_removes_handback() {
    let boot = boot().await;
    let names = calm_server::mcp_server::transport::SessionToolNames::new(
        boot.ctx.clone(),
        boot.registry.clone(),
    );
    for bound in [false, true] {
        if bound {
            child_track_binding(&boot).await;
        }
        let listed = names.listed(PLANNER_SESSION_ID).await.unwrap();
        assert!(listed.iter().any(|n| n == "neige_task_done"));
        assert!(listed.iter().any(|n| n == "neige_task_fail"));
        assert!(!listed.iter().any(|n| n == "neige_track_handback"));
    }
    assert!(boot.registry.lookup("neige_track_handback").is_none());
}

async fn parent_identity(boot: &Boot, parent: &str) -> ToolCallIdentity {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: parent.to_string().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: crate::support::planner::payload(),
        })
        .await
        .unwrap();
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    boot.card_role_cache.insert(
        card.id.clone(),
        CardRole::Planner,
        parent.to_string().into(),
    );
    boot.ctx
        .write
        .remember_track(parent.to_string().into(), boot.area_id.clone());
    seed_track_root_session(
        boot.repo.as_ref(),
        &parent.to_string().into(),
        &card.id,
        "parent-session",
    )
    .await;
    ToolCallIdentity {
        card_id: card.id.to_string(),
        session_id: "parent-session".into(),
        track_id: Some(parent.into()),
        ..planner_identity(boot)
    }
}

#[tokio::test]
async fn child_track_handback_parent_can_close_without_admin_and_foreign_cannot() {
    let boot = boot().await;
    let parent = child_track_binding(&boot).await;
    let identity = parent_identity(&boot, &parent).await;
    let args = json!({"track_id":boot.track_id,"message":"archive returned work"});
    assert!(
        call_tool(&boot, "neige_track_close", identity.clone(), args.clone())
            .await
            .is_err()
    );
    call_tool(
        &boot,
        "neige_task_fail",
        planner_identity(&boot),
        json!({"attempt_id":"assignment","reason":"budget cutoff"}),
    )
    .await
    .unwrap();
    call_tool(&boot, "neige_track_close", identity.clone(), args)
        .await
        .unwrap();
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_some()
    );
    // Repoint the child edge: a non-parent cannot use the parent exception, even in the same Area.
    sqlx::query("UPDATE tracks SET closed_at=NULL,parent_track_id=NULL WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    assert_eq!(
        call_tool(
            &boot,
            "neige_track_close",
            identity,
            json!({"track_id":boot.track_id,"message":"foreign"})
        )
        .await
        .unwrap_err()
        .code,
        -32404
    );
}

#[tokio::test]
async fn child_track_handback_concurrent_calls_emit_one_terminal_event() {
    let boot = boot().await;
    child_track_binding(&boot).await;
    let mut rx = boot.ctx.events.subscribe();
    let (first, second) = tokio::join!(
        call_tool(
            &boot,
            "neige_task_fail",
            planner_identity(&boot),
            json!({"attempt_id":"assignment","reason":"first"})
        ),
        call_tool(
            &boot,
            "neige_task_fail",
            planner_identity(&boot),
            json!({"attempt_id":"assignment","reason":"second"})
        ),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert!(matches!(
        rx.recv().await.unwrap().event,
        Event::TaskFailed { .. }
    ));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn child_track_handback_event_write_failure_rolls_back_parent_flip() {
    let boot = boot().await;
    child_track_binding(&boot).await;
    sqlx::query(
        "CREATE TRIGGER refuse_handback_event BEFORE INSERT ON events \
        WHEN NEW.kind='task.failed' BEGIN SELECT RAISE(ABORT,'event injection'); END",
    )
    .execute(boot.repo.sqlite_db().unwrap().writer_pool())
    .await
    .unwrap();
    let mut rx = boot.ctx.events.subscribe();
    let error = call_tool(
        &boot,
        "neige_task_fail",
        planner_identity(&boot),
        json!({"attempt_id":"assignment","reason":"blocked"}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("event injection"), "{error:?}");
    assert_eq!(
        boot.repo
            .task_get("assignment")
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Running
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn child_track_handback_and_claim_serialize_at_writer() {
    let boot = boot().await;
    child_track_binding(&boot).await;
    insert_task(&boot, "race-work", "race-work", "pending", None).await;
    let db = boot.repo.sqlite_db().unwrap();
    let claim = async {
        // Production's claim primitive under the same IMMEDIATE writer fence as scheduler dispatch.
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&db)
            .await
            .unwrap();
        let changed =
            calm_truth::db::sqlite::task_claim_pending_tx(&mut tx, "race-work", 2, &[], false)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        changed
    };
    let (handback, changed) = tokio::join!(
        call_tool(
            &boot,
            "neige_task_fail",
            planner_identity(&boot),
            json!({"attempt_id":"assignment","reason":"race"})
        ),
        claim,
    );
    let parent = boot.repo.task_get("assignment").await.unwrap().unwrap();
    match handback {
        Ok(_) => {
            assert_eq!(changed, 0);
            assert_eq!(parent.status, calm_server::model::TaskStatus::Failed);
        }
        Err(error) => {
            assert_eq!(error.code, -32409);
            assert_eq!(changed, 1);
            assert_eq!(parent.status, calm_server::model::TaskStatus::Running);
            sqlx::query("UPDATE tasks SET status='done' WHERE id='race-work'")
                .execute(db.writer_pool())
                .await
                .unwrap();
            call_tool(
                &boot,
                "neige_task_fail",
                planner_identity(&boot),
                json!({"attempt_id":"assignment","reason":"settled race"}),
            )
            .await
            .unwrap();
        }
    }
    insert_task(&boot, "race-after", "race-after", "pending", None).await;
    let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    assert_eq!(
        calm_truth::db::sqlite::task_claim_pending_tx(&mut tx, "race-after", 3, &[], false)
            .await
            .unwrap(),
        0
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn child_track_handback_rejects_worker_empty_blocker_and_supplied_references() {
    let boot = boot().await;
    child_track_binding(&boot).await;
    for args in [
        json!({}),
        json!({"attempt_id":"assignment","reason":"  "}),
        json!({"attempt_id":"assignment","reason":"blocked","report":{"card_id":"forged","rev":5}}),
    ] {
        assert_eq!(
            call_tool(&boot, "neige_task_fail", planner_identity(&boot), args)
                .await
                .unwrap_err()
                .code,
            RpcError::INVALID_PARAMS
        );
    }
    assert!(
        call_tool(
            &boot,
            "neige_task_fail",
            worker_identity(&boot),
            json!({"attempt_id":"assignment","reason":"worker"})
        )
        .await
        .is_err()
    );
    let result = call_tool(
        &boot,
        "neige_task_fail",
        planner_identity(&boot),
        json!({"attempt_id":"assignment","reason":"blocked"}),
    )
    .await
    .unwrap();
    assert!(result["candidate"].is_null());
}

#[tokio::test]
async fn child_track_reports_use_parent_seed_attempt() {
    for tool in ["neige_task_done", "neige_task_fail"] {
        let boot = boot().await;
        child_track_binding(&boot).await;
        let args = if tool == "neige_task_done" {
            json!({"attempt_id":"assignment"})
        } else {
            json!({"attempt_id":"assignment","reason":"needs decision"})
        };
        let result = call_tool(&boot, tool, planner_identity(&boot), args).await;
        assert!(result.is_ok(), "{tool}: {result:?}");
    }
}

#[tokio::test]
async fn child_track_active_self_close_uses_task_report() {
    let boot = boot().await;
    child_track_binding(&boot).await;
    let mut rx = boot.ctx.events.subscribe();
    let error = call_tool(
        &boot,
        "neige_track_close",
        planner_identity(&boot),
        json!({"message":"met"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, RpcError::CONFLICT);
    assert!(error.message.contains("neige_task_done"));
    assert!(error.message.contains("neige_task_fail"));
    assert!(rx.try_recv().is_err());
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_none()
    );
}

async fn child_track_report_snapshot(boot: &Boot) -> (String, Option<i64>, i64) {
    sqlx::query_as(
        "SELECT (SELECT status FROM tasks WHERE id='assignment'),closed_at, \
        (SELECT count(*) FROM events) FROM tracks WHERE id=?1",
    )
    .bind(boot.track_id.as_str())
    .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn child_track_reports_reject_wrong_attempt() {
    for tool in ["neige_task_done", "neige_task_fail"] {
        let boot = boot().await;
        child_track_binding(&boot).await;
        let before = child_track_report_snapshot(&boot).await;
        let mut args = json!({"attempt_id":"wrong-attempt"});
        if tool == "neige_task_fail" {
            args["reason"] = json!("blocked");
        }
        assert_eq!(
            call_tool(&boot, tool, planner_identity(&boot), args)
                .await
                .unwrap_err()
                .code,
            RpcError::CONFLICT
        );
        assert_eq!(child_track_report_snapshot(&boot).await, before);
    }
}

#[tokio::test]
async fn child_track_reports_reject_unbound_callers_and_ended_bindings() {
    for tool in ["neige_task_done", "neige_task_fail"] {
        for case in [
            "root",
            "parent",
            "foreign",
            "sibling",
            "retired",
            "assistant",
            "ended",
            "closed",
        ] {
            let boot = boot().await;
            let parent = child_track_binding(&boot).await;
            let db = boot.repo.sqlite_db().unwrap();
            let mut identity = planner_identity(&boot);
            match case {
                "root" => {
                    sqlx::query("UPDATE tracks SET parent_track_id=NULL WHERE id=?1")
                        .bind(boot.track_id.as_str())
                        .execute(db.writer_pool())
                        .await
                        .unwrap();
                }
                "parent" => {
                    identity = parent_identity(&boot, &parent).await;
                }
                "foreign" | "sibling" => {
                    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at,parent_track_id) \
                        VALUES('other',?1,'other',1,1,1,?2)")
                        .bind(boot.area_id.as_str()).bind(if case == "sibling" { Some(parent.as_str()) } else { None }).execute(db.writer_pool()).await.unwrap();
                    identity = parent_identity(&boot, "other").await;
                }
                "retired" => {
                    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?1")
                        .bind(PLANNER_SESSION_ID)
                        .execute(db.writer_pool())
                        .await
                        .unwrap();
                    crate::support::planner::start_session(
                        boot.repo.as_ref(),
                        boot.planner_card_id.as_str(),
                        "replacement",
                    )
                    .await;
                }
                "assistant" => {
                    identity.role = CardRole::Assistant;
                }
                "ended" => {
                    sqlx::query("UPDATE tasks SET status='done' WHERE id='assignment'")
                        .execute(db.writer_pool())
                        .await
                        .unwrap();
                }
                "closed" => {
                    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                        .bind(boot.track_id.as_str())
                        .execute(db.writer_pool())
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let before = child_track_report_snapshot(&boot).await;
            let mut args = json!({"attempt_id":"assignment"});
            if tool == "neige_task_fail" {
                args["reason"] = json!("blocked");
            }
            assert!(
                call_tool(&boot, tool, identity, args).await.is_err(),
                "{case} {tool}"
            );
            assert_eq!(
                child_track_report_snapshot(&boot).await,
                before,
                "{case} {tool}"
            );
        }
    }
}

#[tokio::test]
async fn child_track_done_rejects_work_and_worker_only_fields() {
    for status in ["pending", "dispatched", "running", "verifying"] {
        let boot = boot().await;
        child_track_binding(&boot).await;
        insert_task(&boot, "busy", "busy", status, None).await;
        let before = child_track_report_snapshot(&boot).await;
        assert_eq!(
            call_tool(
                &boot,
                "neige_task_done",
                planner_identity(&boot),
                json!({"attempt_id":"assignment"})
            )
            .await
            .unwrap_err()
            .code,
            -32409
        );
        assert_eq!(child_track_report_snapshot(&boot).await, before);
    }
    let boot = boot().await;
    child_track_binding(&boot).await;
    for field in ["result", "artifacts", "commit_message"] {
        let before = child_track_report_snapshot(&boot).await;
        let mut args = json!({"attempt_id":"assignment"});
        args[field] = Value::Null;
        assert_eq!(
            call_tool(&boot, "neige_task_done", planner_identity(&boot), args)
                .await
                .unwrap_err()
                .code,
            RpcError::INVALID_PARAMS
        );
        assert_eq!(child_track_report_snapshot(&boot).await, before);
    }
}

#[tokio::test]
async fn child_track_done_atomic_result_and_event_rollback() {
    for injection in [
        None,
        Some("task.completed"),
        Some("track.updated"),
        Some("missing-report"),
    ] {
        let boot = boot().await;
        let parent = child_track_binding(&boot).await;
        let db = boot.repo.sqlite_db().unwrap();
        if injection == Some("missing-report") {
            sqlx::query("DELETE FROM cards WHERE track_id=?1 AND kind='track-report'")
                .bind(boot.track_id.as_str())
                .execute(db.writer_pool())
                .await
                .unwrap();
        } else if let Some(kind) = injection {
            sqlx::query(&format!(
                "CREATE TRIGGER refuse_result BEFORE INSERT ON events \
                WHEN NEW.kind='{kind}' BEGIN SELECT RAISE(ABORT,'event injection'); END"
            ))
            .execute(db.writer_pool())
            .await
            .unwrap();
        }
        let expected = if injection.is_none() {
            let report = boot
                .repo
                .cards_by_track(boot.track_id.as_str())
                .await
                .unwrap()
                .into_iter()
                .find(|c| c.kind == "track-report")
                .unwrap();
            let payload = calm_server::track_report::TrackReportPayload::initial();
            let mut doc = calm_server::track_report_doc::ReportDoc::from_payload(&payload);
            doc.increment_doc_rev().unwrap();
            sqlx::query("UPDATE cards SET body_crdt=?1 WHERE id=?2")
                .bind(doc.to_bytes())
                .bind(report.id.as_str())
                .execute(db.writer_pool())
                .await
                .unwrap();
            crate::support::child_track::seed_candidates(
                boot.repo.as_ref(),
                boot.track_id.as_str(),
                report.id.as_str(),
            )
            .await
        } else {
            Value::Null
        };
        let mut rx = boot.ctx.events.subscribe();
        let before = child_track_report_snapshot(&boot).await;
        let result = call_tool(
            &boot,
            "neige_task_done",
            planner_identity(&boot),
            json!({"attempt_id":"assignment"}),
        )
        .await;
        if injection.is_some() {
            assert!(result.is_err());
            assert_eq!(child_track_report_snapshot(&boot).await, before);
        } else {
            let result = result.unwrap();
            assert_eq!(result["outcome"], "met");
            assert_eq!(result["child_track_id"], json!(boot.track_id));
            assert_eq!(result["candidate"], expected);
            assert_eq!(result["report"]["rev"], 1);
            assert!(result["report"]["card_id"].is_string());
            let after = child_track_report_snapshot(&boot).await;
            assert_eq!(after.0, "done");
            assert!(after.1.is_some());
            assert_eq!(after.2, before.2 + 2);
            let close = rx.recv().await.unwrap();
            assert_eq!(close.actor, ActorId::KernelDispatcher);
            assert_eq!(close.scope.track_id().unwrap(), &boot.track_id);
            assert!(matches!(close.event, Event::TrackUpdated(_)));
            let completed = rx.recv().await.unwrap();
            assert_eq!(completed.scope.track_id().unwrap().as_str(), parent);
            assert!(
                matches!(completed.event, Event::TaskCompleted { result: recorded, .. } if recorded == result)
            );
            assert!(rx.try_recv().is_err());
        }
    }
}

#[tokio::test]
async fn child_track_done_concurrent_reports_settle_once() {
    for other in ["neige_task_done", "neige_task_fail"] {
        let boot = boot().await;
        child_track_binding(&boot).await;
        let mut rx = boot.ctx.events.subscribe();
        let mut args = json!({"attempt_id":"assignment"});
        if other == "neige_task_fail" {
            args["reason"] = json!("blocked");
        }
        let (done, second) = tokio::join!(
            call_tool(
                &boot,
                "neige_task_done",
                planner_identity(&boot),
                json!({"attempt_id":"assignment"})
            ),
            call_tool(&boot, other, planner_identity(&boot), args)
        );
        assert_ne!(done.is_ok(), second.is_ok());
        let mut terminal = 0;
        while let Ok(envelope) = rx.try_recv() {
            if matches!(
                envelope.event,
                Event::TaskCompleted { .. } | Event::TaskFailed { .. }
            ) {
                terminal += 1;
            }
        }
        assert_eq!(terminal, 1);
    }
}
