//! Attachment is setup, never evidence for a fresh canonical input.
use super::*;

#[tokio::test]
async fn canonical_acp_failed_load_or_resume_never_creates_replacement() {
    for (scenario, operation) in [
        ("load-error", "session/load"),
        ("resume-error", "session/resume"),
    ] {
        let fx = Fixture::new("lost").await;
        fx.dispatch().await.unwrap();
        fx.wait_idle().await;
        fx.keep_unknown(&fx.submission, "keep").await;
        assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
        let before = fx.receipt_bytes(&fx.submission).await;
        let successor = fx.claim_successor().await;
        std::fs::write(fx.dir.path().join("scenario"), scenario).unwrap();
        let report = fx.dispatch_submission(&successor).await;
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::NotDispatched(_)
        ));
        assert!(report.report.outcome().is_err());
        let calls = fx.requests();
        assert_eq!(calls.iter().filter(|r| r["method"] == operation).count(), 1);
        assert_eq!(
            calls
                .iter()
                .filter(|r| r["method"] == "session/new")
                .count(),
            1
        );
        assert_eq!(fx.prompts(), 1);
        assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
        assert!(fx.complete().await.is_none());
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_load_replay_and_attach_updates_cannot_settle_new_input() {
    let fx = Fixture::new("lost").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    let successor = fx.claim_successor().await;
    std::fs::write(fx.dir.path().join("scenario"), "load-replay-lost").unwrap();
    let mut events = fx.session.subscribe_events();
    let report = fx.dispatch_submission(&successor).await;
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    fx.wait_idle().await;
    let mut observed = Vec::new();
    while let Ok(event) = events.try_recv() {
        observed.push(format!("{event:?}"));
    }
    let observed = observed.join("\n");
    assert!(!observed.contains("historic reply"), "{observed}");
    assert!(!observed.contains("canonical human"), "{observed}");
    let (_, input, state, outcome) = fx.receipt_bytes(&successor).await;
    assert_eq!(state, "unknown");
    assert!(outcome.is_none());
    assert!(!input.contains("canonical human"));
    assert!(fx.complete().await.is_none());
    assert_eq!(
        fx.requests()
            .iter()
            .filter(|r| r["method"] == "session/load")
            .count(),
        1
    );
    assert_eq!(fx.prompts(), 2);
    fx.session.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn canonical_acp_private_server_cleanup_and_cancel_drain_remain_unknown() {
    let fx = Fixture::new("server-drain").await;
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    let server = fx.dir.path().join("private-server");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !server.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(server).unwrap().parse().unwrap();
    let start = crate::proc_identity::read_proc_start_time(pid).expect("private server is live");
    let live = || {
        crate::proc_identity::read_proc_start_time(pid) == Some(start)
            && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| crate::proc_identity::parse_proc_stat_fields(&stat))
                .is_some_and(|stat| stat.state != 'Z')
    };
    assert!(live(), "private serve child must exist before engine Stop");
    let turn = fx
        .session
        .active_turn_id_for_thread(&fx.owner.thread)
        .unwrap();
    let _ = fx.session.turn_interrupt(&fx.owner.thread, &turn).await;
    fx.wait_idle().await;
    // shutdown joins the retained receipt driver after revoke and the process-tree sweep.
    fx.session.shutdown().await.unwrap();
    assert!(!live(), "private server survived engine cleanup");
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert!(token.is_none());
    let (_, _, state, outcome) = fx.receipt_bytes(&fx.submission).await;
    assert_eq!(state, "unknown");
    assert!(outcome.is_none());
    assert!(fx.complete().await.is_none());
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.prompts(), 1);
}

#[tokio::test]
async fn canonical_acp_resume_does_not_require_load_capability() {
    let fx = Fixture::new("lost").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    let successor = fx.claim_successor().await;
    std::fs::write(fx.dir.path().join("scenario"), "resume-only").unwrap();
    let report = fx.dispatch_submission(&successor).await;
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    assert!(report.report.outcome().is_ok());
    fx.wait_idle().await;
    assert_eq!(fx.receipt_bytes(&successor).await.2, "completed");
    fx.wait_end().await;
    let executions: i64 = sqlx::query_scalar(concat!(
        "SELECT count(*) FROM workspace_executions WHERE ",
        "json_extract(intent_json,'$.consumer')='acp-planner' ",
        "AND desired='retire' AND settled_at_ms IS NOT NULL",
    ))
    .fetch_one(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_eq!(executions, 2, "new and resume both settle through Engine");
    let calls = fx.requests();
    assert_eq!(
        calls
            .iter()
            .filter(|r| r["method"] == "session/resume")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|r| r["method"] == "session/load")
            .count(),
        0
    );
    assert_eq!(
        calls
            .iter()
            .filter(|r| r["method"] == "session/new")
            .count(),
        1
    );
    fx.session.shutdown().await.unwrap();
}
