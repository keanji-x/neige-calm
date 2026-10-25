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
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,child_track_id,created_at_ms,updated_at_ms) VALUES('assignment',?1,'assignment','codex','goal','{}','running','sub-track',?2,1,1)")
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
        "neige_track_handback",
        planner_identity(&boot),
        json!({"blocked_on":"Need a decision"}),
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
            "neige_track_handback",
            planner_identity(&boot),
            json!({"blocked_on":"again"})
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
            "neige_track_handback",
            planner_identity(&boot),
            json!({"blocked_on":"Need a decision"}),
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
async fn child_track_handback_catalog_is_child_planner_only() {
    let boot = boot().await;
    let names = calm_server::mcp_server::transport::SessionToolNames::new(
        boot.ctx.clone(),
        boot.registry.clone(),
    );
    assert!(
        !names
            .listed(PLANNER_SESSION_ID)
            .await
            .unwrap()
            .iter()
            .any(|n| n == "neige_track_handback")
    );
    child_track_binding(&boot).await;
    assert!(
        names
            .listed(PLANNER_SESSION_ID)
            .await
            .unwrap()
            .iter()
            .any(|n| n == "neige_track_handback")
    );
    assert!(
        !boot
            .registry
            .descriptors_listed_for(CardRole::Worker)
            .iter()
            .any(|d| d.name == "neige_track_handback")
    );
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
        "neige_track_handback",
        planner_identity(&boot),
        json!({"blocked_on":"budget cutoff"}),
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
            "neige_track_handback",
            planner_identity(&boot),
            json!({"blocked_on":"first"})
        ),
        call_tool(
            &boot,
            "neige_track_handback",
            planner_identity(&boot),
            json!({"blocked_on":"second"})
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
    sqlx::query("CREATE TRIGGER refuse_handback_event BEFORE INSERT ON events WHEN NEW.kind='task.failed' BEGIN SELECT RAISE(ABORT,'event injection'); END")
        .execute(boot.repo.sqlite_db().unwrap().writer_pool()).await.unwrap();
    let mut rx = boot.ctx.events.subscribe();
    let error = call_tool(
        &boot,
        "neige_track_handback",
        planner_identity(&boot),
        json!({"blocked_on":"blocked"}),
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
            "neige_track_handback",
            planner_identity(&boot),
            json!({"blocked_on":"race"})
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
                "neige_track_handback",
                planner_identity(&boot),
                json!({"blocked_on":"settled race"}),
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
        json!({"blocked_on":"  "}),
        json!({"blocked_on":"blocked","report":{"card_id":"forged","rev":5}}),
    ] {
        assert_eq!(
            call_tool(&boot, "neige_track_handback", planner_identity(&boot), args)
                .await
                .unwrap_err()
                .code,
            RpcError::INVALID_PARAMS
        );
    }
    assert!(
        call_tool(
            &boot,
            "neige_track_handback",
            worker_identity(&boot),
            json!({"blocked_on":"worker"})
        )
        .await
        .is_err()
    );
    let result = call_tool(
        &boot,
        "neige_track_handback",
        planner_identity(&boot),
        json!({"blocked_on":"blocked"}),
    )
    .await
    .unwrap();
    assert!(result["candidate"].is_null());
}
