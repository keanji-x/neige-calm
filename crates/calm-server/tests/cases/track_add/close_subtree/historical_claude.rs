use super::*;

#[tokio::test]
async fn detached_historical_claude_stays_pending_without_boot_absence() {
    check_detached_claude_cleanup(true).await;
}

#[tokio::test]
async fn detached_historical_claude_retires_with_boot_absence() {
    check_detached_claude_cleanup(false).await;
}

async fn check_detached_claude_cleanup(unproven: bool) {
    use calm_server::harness::{HarnessPhaseTag, HarnessSnapshot};
    use calm_server::model::{NewTrack, RequestTheme};
    let boot = boot(16).await;
    let state = &boot.native.as_ref().unwrap().state;
    let track = boot
        .repo
        .track_create(NewTrack {
            area_id: boot.area_id.clone().into(),
            title: "historical Claude".into(),
            cwd: boot._tmp.path().to_string_lossy().into_owned(),
            sort: None,
            attach_folder: true,
            template_id: None,
            template_input: None,
            plugin_scope: None,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "agent".into(),
            sort: None,
            payload: json!({"planner_provider":"claude","planner_harness":true,"schemaVersion":1}),
        })
        .await
        .unwrap();
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    let worker = "historical-claude-session";
    let mut original = session(
        worker,
        track.id.as_str(),
        card.id.as_str(),
        WorkerContract::Planner,
    );
    original.provider = WorkerProviderKind::Claude;
    original.state = WorkerSessionState::Running;
    original.thread_id = Some("historical-thread".into());
    let mut frame = HarnessSnapshot::initial(0, vec![]);
    frame.phase = HarnessPhaseTag::Idle;
    frame.last_thread_id = original.thread_id.clone();
    original.handle_state_json = Some(serde_json::to_value(frame).unwrap());
    boot.seed_session(original).await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                .bind(worker)
                .bind(card.id.as_str()),
        )
        .await
        .unwrap();
    let unconfirmed = if unproven {
        vec![worker.to_owned()]
    } else {
        vec![]
    };
    calm_server::test_seams::planner_upgrade_unproven_for_test(state, &unconfirmed, &[]);
    assert!(state.harness.get(worker).is_none());
    let fence = boot.ctx.tool_calls.fence(worker).unwrap();
    assert_eq!(rest_close(&boot, track.id.as_str()).await, StatusCode::OK);
    let count = "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1";
    assert_eq!(boot.scalar::<i64>(count, worker).await, 1);
    // Let the runtime register the pending cleanup, then release only its carrier.
    calm_server::terminal_sweeper::sweep(state).await.unwrap();
    drop(fence);
    let caller = boot.caller(
        track.id.as_str(),
        card.id.as_str(),
        CardRole::Planner,
        worker,
    );
    let _after = cleanup_pass_finished(&boot, &caller).await;
    let after: i64 = boot.scalar(count, worker).await;
    assert_eq!(after, i64::from(unproven));
}
