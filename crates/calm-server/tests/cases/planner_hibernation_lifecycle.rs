use super::*;

async fn close_track(boot: &Boot) {
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&boot.track_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
}

async fn failed_release_after_owner_change(change: &str) {
    let _env = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    let failure = sock.with_extension("unsubscribe-fail");
    std::fs::write(&failure, "fail").unwrap();
    close_track(&boot).await;
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert!(boot.state.harness.get(&runtime).is_none());
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        None
    );
    if change == "reset" {
        // The wire fixture defaults every new thread to the same id. A real reset
        // creates a different thread; keep the old release obligation distinct.
        std::fs::write(
            sock.with_extension("thread-start"),
            json!({"thread":{"id":"reset-new-thread"},"model":"fake-model"}).to_string(),
        )
        .unwrap();
    }
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(if change == "reset" { "POST" } else { "DELETE" })
                .uri(if change == "reset" {
                    format!("/api/cards/{}/planner/reset", card.id)
                } else if change == "card_delete" {
                    format!("/api/cards/{}", card.id)
                } else {
                    format!("/api/tracks/{}", boot.track_id)
                })
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{change}: {}",
        response.status()
    );
    let before = std::fs::read_to_string(sock.with_extension("unsubscribed")).unwrap();
    std::fs::remove_file(failure).unwrap();
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    let released = std::fs::read_to_string(sock.with_extension("unsubscribed")).unwrap();
    assert_eq!(
        released.lines().filter(|line| *line == thread).count(),
        before.lines().filter(|line| *line == thread).count() + 1,
        "failed release must survive {change} without an eligible carrier"
    );
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(sock.with_extension("unsubscribed"))
            .unwrap()
            .lines()
            .filter(|line| *line == thread)
            .count(),
        released.lines().filter(|line| *line == thread).count(),
        "successful retry must retire its obligation"
    );
}

#[tokio::test]
async fn planner_hibernat_failed_release_survives_reset() {
    failed_release_after_owner_change("reset").await;
}
#[tokio::test]
async fn planner_hibernat_failed_release_survives_track_delete() {
    failed_release_after_owner_change("track_delete").await;
}
#[tokio::test]
async fn planner_hibernat_failed_release_survives_card_delete() {
    failed_release_after_owner_change("card_delete").await;
}

async fn send_completed(boot: &Boot, card: &Card, runtime: &String, text: &str) {
    let (status, body) = post_planner_input(
        boot.app.clone(),
        &format!("/api/cards/{}/planner/input", card.id),
        json!({"text":format!("fake-reply: {text}")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], runtime.as_str());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let rows = conversation_rows(boot, card).await;
        let answered = rows.iter().any(|r| {
            r["params"].as_str().is_some_and(|p| p.contains(text))
                && r["method"] == "item/completed"
        });
        let handle = boot.state.harness.get(runtime).unwrap();
        if answered && handle.snapshot().await.unwrap().phase == HarnessPhaseTag::TurnCompleted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "send did not complete: {text}; block={:?}; snapshot={:?}; rows={rows:?}",
            handle.issuance_block().await,
            handle.snapshot().await.unwrap()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn planner_hibernat_live_closed_owner_survives_daemon_replace() {
    use calm_server::shared_codex_appserver::ReplacePrecondition;
    let _env = ENV_LOCK.lock().await;
    let boot = boot_shared().await;
    let (card, runtime, handle) = loaded_planner(&boot).await;
    let thread = handle.thread_id_for_test().await.unwrap();
    sqlx::query("UPDATE cards SET payload=json_patch(payload,?) WHERE id=?")
        .bind(
            json!({"model":"gpt-5","reasoning_effort":"high","permission_mode":"never"})
                .to_string(),
        )
        .bind(card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    close_track(&boot).await;
    calm_server::terminal_sweeper::sweep(&boot.state)
        .await
        .unwrap();
    let sock = boot._tmp.path().join("run/codex-appserver.sock");
    std::fs::write(sock.with_extension("require-resume"), "1").unwrap();
    let idle = json!({"thread":{"id":thread,"status":{"type":"idle"}}}).to_string();
    std::fs::write(sock.with_extension("thread-read"), &idle).unwrap();
    std::fs::write(sock.with_extension("thread-resume"), &idle).unwrap();
    send_completed(&boot, &card, &runtime, "before replacement").await;
    boot.state
        .shared_codex_appserver
        .transition_replace_for_test("live closed planner", ReplacePrecondition::Always)
        .await
        .unwrap();
    send_completed(&boot, &card, &runtime, "after replacement").await;
    assert_eq!(
        boot.state
            .shared_codex_appserver
            .cached_card_for_thread(&thread),
        Some(card.id.to_string()),
        "live owner must keep attribution before the next sweep"
    );
    assert_eq!(
        boot.state
            .harness
            .get(&runtime)
            .unwrap()
            .thread_id_for_test()
            .await,
        Some(thread)
    );
    boot.state
        .harness
        .shutdown_and_remove(&runtime)
        .await
        .unwrap();
}
