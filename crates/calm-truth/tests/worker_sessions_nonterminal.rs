use calm_truth::db::RepoSyncDomainRaw;
use calm_truth::db::sqlite::{
    SqlxRepo, begin_immediate_tx, session_insert_tx, session_state_transition_tx,
};
use calm_truth::model::{NewCove, NewWave, RequestTheme};
use calm_truth::session_repo::SessionRepo;
use calm_types::ids::WaveId;
use calm_types::worker::{
    Liveness, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
    WorkerSessionId, WorkerSessionState,
};

async fn seeded_repo() -> (SqlxRepo, WaveId) {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    let cove = repo
        .cove_create(NewCove {
            name: "worker-session-scan".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("seed cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "worker-session-scan".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("seed wave");
    (repo, wave.id)
}

fn session(state: WorkerSessionState, wave_id: WaveId, created_at_ms: i64) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(format!("ws-{}", state.as_db_str())),
        wave_id,
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Executor,
        parent_session_id: None,
        requester_session_id: None,
        state,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        created_at_ms,
        updated_at_ms: created_at_ms,
        completed_at_ms: state.is_terminal().then_some(created_at_ms),
    }
}

#[tokio::test]
async fn sessions_nonterminal_matches_worker_session_state_terminal_set() {
    let (repo, wave_id) = seeded_repo().await;
    let states = [
        WorkerSessionState::Starting,
        WorkerSessionState::Running,
        WorkerSessionState::Idle,
        WorkerSessionState::TurnPending,
        WorkerSessionState::Exited,
        WorkerSessionState::Failed,
        WorkerSessionState::Superseded,
    ];
    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    for (idx, state) in states.into_iter().enumerate() {
        session_insert_tx(&mut tx, session(state, wave_id.clone(), idx as i64 + 1))
            .await
            .expect("insert session");
    }
    tx.commit().await.expect("commit tx");

    let got = repo
        .sessions_nonterminal()
        .await
        .expect("list non-terminal sessions")
        .into_iter()
        .map(|session| session.state)
        .collect::<Vec<_>>();
    let expected = states
        .into_iter()
        .filter(|state| !state.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn session_set_liveness_preserves_updated_at_ms() {
    let (repo, wave_id) = seeded_repo().await;
    let id = WorkerSessionId::from("ws-liveness-preserves-updated-at");
    let created_at_ms = 1_000;
    let updated_at_ms = 1_234;
    let probed_at_ms = 9_876;
    let mut seeded = session(WorkerSessionState::Running, wave_id, created_at_ms);
    seeded.id = id.clone();
    seeded.updated_at_ms = updated_at_ms;

    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    session_insert_tx(&mut tx, seeded)
        .await
        .expect("insert session");
    tx.commit().await.expect("commit tx");

    let written = repo
        .session_set_liveness(&id, &Liveness::Idle, probed_at_ms)
        .await
        .expect("set liveness")
        .expect("active session was updated");
    assert_eq!(written.liveness, LivenessTag::Idle);
    assert_eq!(written.liveness_probed_at_ms, Some(probed_at_ms));
    assert_eq!(written.updated_at_ms, updated_at_ms);

    let row = repo
        .session_get(&id)
        .await
        .expect("get session")
        .expect("session exists");
    assert_eq!(row.liveness, LivenessTag::Idle);
    assert_eq!(row.liveness_probed_at_ms, Some(probed_at_ms));
    assert_eq!(row.updated_at_ms, updated_at_ms);
}

#[tokio::test]
async fn session_set_liveness_noops_after_terminal_transition() {
    let (repo, wave_id) = seeded_repo().await;
    let id = WorkerSessionId::from("ws-liveness-terminal-race");
    let created_at_ms = 2_000;
    let active_probe_ms = 2_500;
    let stale_probe_ms = 3_000;
    let mut seeded = session(WorkerSessionState::Running, wave_id, created_at_ms);
    seeded.id = id.clone();

    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    session_insert_tx(&mut tx, seeded)
        .await
        .expect("insert session");
    tx.commit().await.expect("commit insert tx");

    let active_write = repo
        .session_set_liveness(&id, &Liveness::Idle, active_probe_ms)
        .await
        .expect("set active liveness")
        .expect("active session was updated");
    assert_eq!(active_write.liveness, LivenessTag::Idle);
    assert_eq!(active_write.liveness_probed_at_ms, Some(active_probe_ms));

    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    let terminal = session_state_transition_tx(&mut tx, &id, WorkerSessionState::Exited)
        .await
        .expect("transition session to exited");
    tx.commit().await.expect("commit transition tx");
    assert_eq!(terminal.state, WorkerSessionState::Exited);
    assert_eq!(terminal.liveness, LivenessTag::Idle);

    let stale_write = repo
        .session_set_liveness(
            &id,
            &Liveness::Alive {
                active_turn_id: None,
            },
            stale_probe_ms,
        )
        .await
        .expect("stale liveness write is benign");
    assert!(stale_write.is_none());

    let row = repo
        .session_get(&id)
        .await
        .expect("get session")
        .expect("session exists");
    assert_eq!(row.state, WorkerSessionState::Exited);
    assert_eq!(row.liveness, LivenessTag::Idle);
    assert_ne!(row.liveness, LivenessTag::Alive);
    assert_eq!(row.liveness_probed_at_ms, Some(active_probe_ms));
}
