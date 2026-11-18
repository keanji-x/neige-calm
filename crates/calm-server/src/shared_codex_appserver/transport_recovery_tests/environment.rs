use super::*;

#[tokio::test]
async fn cold_replay_metadata_wait_rechecks_retired_owner_before_project_evaluation() {
    let f = fixture().await;
    let (card, runtime) = seed_cold_owner(&f).await;
    let source = f._root.path();
    std::fs::write(
        source.join(".envrc"),
        "printf evaluated >> \"$PWD/environment-evaluated\"\n",
    )
    .unwrap();
    assert!(
        std::process::Command::new("direnv")
            .arg("allow")
            .arg(source)
            .env_clear()
            .envs(crate::workspace_service::environment::directory_base_environment())
            .output()
            .unwrap()
            .status
            .success()
    );
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("codex-replay-cold-prepared", &card, pause.clone());
    let client = f.daemon.running_client().await.unwrap();
    let daemon = f.daemon.clone();
    let replay = tokio::spawn(async move { daemon.connected_client().await });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    let sink = client.lock_outbound_sink_for_test().await;
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    write_in_tx_typed(f.daemon.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_fail_if_active_runtime_tx(tx, &runtime).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    drop(sink);
    tokio::time::timeout(Duration::from_secs(5), replay)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!source.join("environment-evaluated").exists());
    assert!(
        !f.requests.lock().unwrap().iter().any(|request| {
            request["method"] == "config/read" || request["method"] == "thread/resume"
        }),
        "retiring the prepared owner must refuse every subsequent private RPC"
    );
}

#[tokio::test]
async fn cancelled_cold_replay_retains_a_now_loaded_context_without_reloading_direnv() {
    let f = fixture().await;
    seed_cold_owner(&f).await;
    cancel_after_cold_resume_send(&f).await;
    std::fs::write(
        f._root.path().join(".envrc"),
        "export GH_TOKEN=synthetic-unapproved-after-cold-send\n",
    )
    .unwrap();
    f.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({
            "result":{"thread":{"id":"cold-thread","status":{"type":"idle"},"turns":[]}}
        }),
    );
    f.daemon.connected_client().await.unwrap();
    let requests = f.requests.lock().unwrap();
    let resumes = requests
        .iter()
        .filter(|request| request["method"] == "thread/resume")
        .collect::<Vec<_>>();
    assert_eq!(resumes.len(), 2);
    assert!(
        resumes[0]
            .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
            .is_some()
    );
    assert!(resumes[1]["params"].get("config").is_none());
    assert!(
        !requests
            .iter()
            .any(|request| request["method"] == "turn/start")
    );
}

#[tokio::test]
async fn hot_replay_retains_loaded_context_when_workspace_environment_is_unavailable() {
    let f = fixture().await;
    let source = f._root.path().join("unapproved-project");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        source.join(".envrc"),
        "export GH_TOKEN=synthetic-unapproved-reconnect\n",
    )
    .unwrap();
    let area = f
        .daemon
        .repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "reconnect".into(),
                color: "#000000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = f
        .daemon
        .repo
        .track_create(crate::model::NewTrack {
            area_id: area.id,
            title: "reconnect".into(),
            cwd: source.display().to_string(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = f.daemon.repo.card_create(crate::model::NewCard {
        track_id: track.id, title: None, kind: "terminal".into(), sort: None,
        payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false},"codex_source":"shared"}),
    }).await.unwrap();
    f.daemon
        .thread_cache
        .insert("retained-thread".into(), card.id.to_string());
    {
        let mut core = f.daemon.core.lock().await;
        let SupervisorState::Running { replay, .. } = &mut core.state else {
            panic!("running fixture")
        };
        *replay = ConnectionReplay::Pending(ReplayPlan::default());
    }
    f.daemon.resume_cached_threads().await;
    let requests = f.requests.lock().unwrap();
    let resumes = requests
        .iter()
        .filter(|request| {
            request["method"] == "thread/resume"
                && request["params"]["threadId"] == "retained-thread"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        resumes.len(),
        1,
        "a loaded context must regain its subscription"
    );
    assert!(resumes[0]["params"].get("config").is_none());
    assert!(
        !requests
            .iter()
            .any(|request| request["method"] == "config/read" || request["method"] == "turn/start")
    );
}
