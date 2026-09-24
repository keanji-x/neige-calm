//! #1791: the Codex recovery path only recovers a Codex Planner row on a Planner card that binds
//! Codex; a Claude row, or a row whose card names another (or no) backend, is skipped.

use super::*;

async fn recover_planner_row(
    card_provider: Option<&str>,
    row_provider: AgentProvider,
) -> (calm_server::harness::RecoveryOutcome, bool) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "planner-provider-recovery".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "planner provider".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut payload = json!({"schemaVersion": 1, "planner_harness": true});
    if let Some(provider) = card_provider {
        payload["planner_provider"] = json!(provider);
    }
    let thread_id = format!("thread-{}", new_id());
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload,
        },
        CardRole::Planner,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    let runtime_id = new_id();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit::shared_planner(
            runtime_id.clone(),
            card.id.to_string(),
            row_provider,
            WorkerSessionState::Idle,
            Some(thread_id),
            serde_json::to_value(&snapshot).unwrap(),
            now_ms(),
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let runtime = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
        .unwrap()
        .unwrap();
    let registry = HarnessRegistry::new();
    let outcome = spawn_recovered_harness(
        repo.clone(),
        EventBus::new(),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
        runtime,
        ClaimMode::Replace,
    )
    .await
    .unwrap();
    let installed = registry.get(&runtime_id).is_some();
    if let Some(handle) = registry.remove(&runtime_id) {
        handle.shutdown().await.unwrap();
    }
    (outcome, installed)
}

#[tokio::test]
async fn a_codex_planner_row_on_a_codex_planner_card_is_recovered() {
    let (_, installed) = recover_planner_row(Some("codex"), AgentProvider::Codex).await;
    assert!(installed, "the Codex control case must recover");
}

#[tokio::test]
async fn a_planner_row_the_codex_path_cannot_run_is_skipped() {
    for (card_provider, row_provider) in [
        (Some("claude"), AgentProvider::Claude),
        (Some("codex"), AgentProvider::Claude),
        (Some("claude"), AgentProvider::Codex),
        (None, AgentProvider::Codex),
        (Some("gpt"), AgentProvider::Codex),
    ] {
        let (outcome, installed) = recover_planner_row(card_provider, row_provider.clone()).await;
        assert!(
            matches!(outcome, calm_server::harness::RecoveryOutcome::Skipped),
            "card={card_provider:?} row={row_provider:?}"
        );
        assert!(!installed, "card={card_provider:?} row={row_provider:?}");
    }
}
