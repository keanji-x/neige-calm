//! #1853: the kernel's one connection to the shared daemon releases (`thread/unsubscribe`) a
//! thread whose Card was deleted or whose session POSITIVELY ended, so codex can unload it and
//! its MCP servers. A live session's thread, and a freshly minted thread with no session row
//! yet, stay subscribed. Real wire: the fixture appends each `threadId` to `<sock>.unsubscribed`.
use super::*;

use std::collections::HashSet;

use calm_server::db::sqlite::{session_complete_for_card_tx, session_supersede_and_start_tx};
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::WorkerSessionProjectionRepo;
use calm_server::state::{AppState, CodexClient, DaemonClient};

fn unsubscribed_on_wire(root: &tempfile::TempDir) -> Vec<String> {
    let path = root
        .path()
        .join("run/codex-appserver.sock")
        .with_extension("unsubscribed");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn state_with(repo: Arc<SqlxRepo>, daemon: Arc<SharedCodexAppServer>) -> AppState {
    let repo: Arc<dyn Repo> = repo;
    AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
    .with_shared_codex_appserver(daemon)
}

fn running_init(card_id: &str, thread_id: &str) -> WorkerSessionInit {
    WorkerSessionInit {
        id: new_id(),
        card_id: card_id.to_string(),
        kind: WorkerSessionKind::CodexCard,
        agent_provider: Some(AgentProvider::Codex),
        status: WorkerSessionState::Running,
        terminal_run_id: None,
        thread_id: Some(thread_id.to_string()),
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    }
}

#[tokio::test]
async fn a_committed_delete_cleanup_unsubscribes_the_deleted_cards_threads_on_the_wire() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let victim = seed_card(&repo, 0).await;
    seed_runtime_thread(&repo, &victim, "thread-victim").await;
    let survivor = seed_card(&repo, 1).await;
    seed_runtime_thread(&repo, &survivor, "thread-survivor").await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    assert_eq!(unsubscribed_on_wire(&root), Vec::<String>::new(), "premise");

    let dropped = daemon
        .forget_threads_for_deleted_cards(&HashSet::from([victim]))
        .await;

    assert_eq!(dropped, 1);
    assert_eq!(unsubscribed_on_wire(&root), vec!["thread-victim"]);
    assert_eq!(daemon.cached_card_for_thread("thread-victim"), None);
    assert_eq!(
        daemon.cached_card_for_thread("thread-survivor"),
        Some(survivor)
    );
}

#[tokio::test]
async fn the_sweep_releases_only_threads_whose_session_positively_ended() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let ended = seed_card(&repo, 0).await;
    seed_runtime_thread(&repo, &ended, "thread-ended").await;
    let running = seed_card(&repo, 1).await;
    seed_runtime_thread(&repo, &running, "thread-running").await;
    let idle = seed_card(&repo, 2).await;
    seed_runtime_thread(&repo, &idle, "thread-idle").await;
    // A superseded row whose thread a newer live row carries on.
    let carried = seed_card(&repo, 3).await;
    let carried_old = seed_runtime_thread(&repo, &carried, "thread-carried").await;
    // Superseded by a forced new thread; a failed start would restore this row.
    let superseded = seed_card(&repo, 5).await;
    let superseded_old = seed_runtime_thread(&repo, &superseded, "thread-superseded").await;
    let unowned = seed_card(&repo, 4).await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    // A freshly minted thread sits in the cache before any session row names it.
    let minted = daemon
        .thread_start_mint_for_card(
            &unowned,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();

    let write_db = (repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin().await.unwrap();
                session_complete_for_card_tx(&mut tx, &ended, WorkerSessionState::Exited)
                    .await
                    .unwrap();
                session_supersede_and_start_tx(
                    &mut tx,
                    &carried_old,
                    running_init(&carried, "thread-carried"),
                )
                .await
                .unwrap();
                session_supersede_and_start_tx(
                    &mut tx,
                    &superseded_old,
                    running_init(&superseded, "thread-successor"),
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                repo.session_projection_set_status_for_card(&idle, WorkerSessionState::Idle)
                    .await
                    .unwrap();
                let kept = [
                    ("thread-running", &running),
                    ("thread-idle", &idle),
                    ("thread-carried", &carried),
                    ("thread-superseded", &superseded),
                    (minted.as_str(), &unowned),
                ];
                for (thread_id, card_id) in kept.iter().chain([&("thread-ended", &ended)]) {
                    assert_eq!(
                        daemon.cached_card_for_thread(thread_id).as_ref(),
                        Some(*card_id),
                        "premise: {thread_id} is cached"
                    );
                }

                let state = state_with(repo.clone(), daemon.clone());
                calm_server::terminal_sweeper::sweep(&state).await.unwrap();

                assert_eq!(unsubscribed_on_wire(&root), vec!["thread-ended"]);
                assert_eq!(daemon.cached_card_for_thread("thread-ended"), None);
                for (thread_id, card_id) in kept {
                    assert_eq!(
                        daemon.cached_card_for_thread(thread_id).as_ref(),
                        Some(card_id),
                        "{thread_id} must stay attributed and subscribed"
                    );
                }

                // Released once: the next tick has nothing left to release.
                calm_server::terminal_sweeper::sweep(&state).await.unwrap();
                assert_eq!(unsubscribed_on_wire(&root), vec!["thread-ended"]);
            })
        })
        .await
        .expect("independent write operation")
}
