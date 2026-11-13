use super::*;
#[tokio::test]
async fn legacy_disposal_exited_row_unrelated_pid_track_delete() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let dir = calm_test_sockets::socket_dir("ld");
    let sock = dir.path().join("owner.sock");
    let listener = calm_proc_supervisor::bind_control_listener(&sock).unwrap();
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(calm_proc_supervisor::serve_with_listener(
        listener,
        sock.clone(),
        calm_proc_supervisor::ProcRegistry::new(),
        stopped,
    ));
    let state = state_with_daemon(
        repo.clone(),
        DaemonClient {
            data_dir: dir.path().into(),
            proc_supervisor_sock: Some(sock),
        },
    );
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "legacy".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "legacy".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo.card_create(NewCard { track_id: track.id.clone(), title: None, kind: "terminal".into(), sort: None, payload: json!({"terminal_presentation": {"source": "unrecorded", "in_workbench": false}}) }).await.unwrap();
    let term = repo
        .terminal_create(NewTerminal {
            card_id: card.id,
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut unrelated = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    repo.terminal_set_pid(&term.id, unrelated.id())
        .await
        .unwrap();
    repo.terminal_set_exit(&term.id, Some(2), false)
        .await
        .unwrap();
    let response = build_app(state)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tracks/{}", track.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "historical pid must never authorize a signal"
    );
    let status = response.status();
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(repo.terminal_get(&term.id).await.unwrap().is_none());
}

pub(super) struct Owner {
    registry: calm_proc_supervisor::ProcRegistry,
    _dir: tempfile::TempDir,
    pub(super) sock: std::path::PathBuf,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}
impl Owner {
    pub(super) fn new() -> Self {
        let dir = calm_test_sockets::socket_dir("ld");
        let sock = dir.path().join("owner.sock");
        let listener = calm_proc_supervisor::bind_control_listener(&sock).unwrap();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let registry = calm_proc_supervisor::ProcRegistry::new();
        let task = tokio::spawn(calm_proc_supervisor::serve_with_listener(
            listener,
            sock.clone(),
            registry.clone(),
            stopped,
        ));
        Self {
            registry,
            _dir: dir,
            sock,
            task,
            _shutdown: shutdown,
        }
    }
    pub(super) fn state(&self, repo: Arc<dyn Repo>) -> AppState {
        state_with_daemon(
            repo,
            DaemonClient {
                data_dir: self._dir.path().into(),
                proc_supervisor_sock: Some(self.sock.clone()),
            },
        )
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.registry.terminate_all_process_groups_sync();
        self.task.abort();
    }
}

async fn seed(state: &AppState) -> (String, String, String, String) {
    let repo = state.raw_repo();
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "legacy".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "legacy".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false}}),
        })
        .await
        .unwrap();
    let term = repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    (
        area.id.to_string(),
        track.id.to_string(),
        card.id.to_string(),
        term.id,
    )
}
async fn delete(state: &AppState, path: String) -> axum::response::Response {
    build_app(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn legacy_disposal_unknown_proc_exit_signal_and_no_exit_all_scopes() {
    for scope in ["tracks", "areas", "cards", "orphan"] {
        for exit in [Some((Some(2), false)), Some((None, true)), None] {
            let owner = Owner::new();
            let concrete = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
            let state = owner.state(concrete.clone());
            let (area, track, card, term) = seed(&state).await;
            let mut unrelated = tokio::process::Command::new("/bin/sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            state
                .repo
                .terminal_set_pid(&term, unrelated.id())
                .await
                .unwrap();
            if let Some((code, signal)) = exit {
                state
                    .repo
                    .terminal_set_exit(&term, code, signal)
                    .await
                    .unwrap();
            }
            if scope == "orphan" {
                concrete
                    .db()
                    .writer()
                    .execute(
                        sqlx::query("UPDATE cards SET kind='plugin:orphan:test' WHERE id=?1")
                            .bind(&card),
                    )
                    .await
                    .unwrap();
                concrete
                    .db()
                    .writer()
                    .execute(
                        sqlx::query("UPDATE terminals SET created_at=0 WHERE id=?1").bind(&term),
                    )
                    .await
                    .unwrap();
                calm_server::terminal_sweeper::sweep(&state).await.unwrap();
            } else {
                let id = match scope {
                    "tracks" => &track,
                    "areas" => &area,
                    _ => &card,
                };
                assert_eq!(
                    delete(&state, format!("/api/{scope}/{id}")).await.status(),
                    StatusCode::NO_CONTENT,
                    "{scope} {exit:?}"
                );
            }
            assert!(
                unrelated.try_wait().unwrap().is_none(),
                "{scope} must not signal historical pid"
            );
            assert!(
                state.repo.terminal_get(&term).await.unwrap().is_none(),
                "{scope} {exit:?}"
            );
            unrelated.kill().await.unwrap();
            unrelated.wait().await.unwrap();
        }
    }
}

#[tokio::test]
async fn legacy_disposal_unreachable_owner_retains_exited_rows() {
    let owner = Owner::new();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo);
    let (_, track, _, term) = seed(&state).await;
    state
        .repo
        .terminal_set_exit(&term, Some(2), false)
        .await
        .unwrap();
    owner.task.abort();
    tokio::task::yield_now().await;
    assert_eq!(
        delete(&state, format!("/api/tracks/{track}"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
    assert!(state.repo.track_get(&track).await.unwrap().is_some());
}

#[tokio::test]
async fn legacy_disposal_live_owner_stop_overrides_exit_projection() {
    let owner = Owner::new();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo);
    let (_, _, card, term) = seed(&state).await;
    let entry = owned_entry(&state, &owner.sock, &term).await;
    state
        .repo
        .terminal_set_exit(&term, Some(2), false)
        .await
        .unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        entry
            .wait_exit_persisted_for_test(Duration::from_secs(1))
            .await
    );
    assert!(state.terminal_renderer.get(&term).is_none());
}

pub(super) async fn owned_entry(
    state: &AppState,
    socket: &std::path::Path,
    terminal: &str,
) -> Arc<calm_server::terminal_renderer::RendererEntry> {
    state
        .terminal_renderer
        .ensure(calm_server::terminal_renderer::RendererConfig {
            terminal_id: terminal.into(),
            cols: 80,
            rows: 24,
            buffer_bytes: 8192,
            terminal_fg: (220, 220, 220),
            terminal_bg: (15, 20, 24),
            program: "/bin/sleep".into(),
            args: vec!["60".into()],
            envs: vec![],
            cwd: "/tmp".into(),
            supervisor_sock: socket.into(),
        })
        .await
        .unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Refusal {
    Internal,
    OldVersion,
    NoExited,
    Eof,
    Signal,
    Protocol,
    Exited,
}
struct MockOwner {
    _dir: tempfile::TempDir,
    sock: std::path::PathBuf,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MockOwner {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl MockOwner {
    fn new(mode: Refusal) -> Self {
        use calm_session::control::{Attached, ControlErrorKind, ControlMsg, ControlReply};
        use calm_session::{read_frame, write_frame};
        let dir = calm_test_sockets::socket_dir("ld-mock");
        let sock = dir.path().join("owner.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let task = tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(ControlMsg::Probe(probe)) = read_frame::<ControlMsg, _>(&mut conn).await
                    else {
                        return;
                    };
                    assert!(probe.proc_id.is_empty());
                    write_frame(
                        &mut conn,
                        &ControlReply::ProbeOk {
                            supervisor_version: if matches!(mode, Refusal::OldVersion) {
                                1
                            } else {
                                2
                            },
                            proc_running: false,
                        },
                    )
                    .await
                    .unwrap();
                    while let Ok(msg) = read_frame::<ControlMsg, _>(&mut conn).await {
                        match msg {
                            ControlMsg::Attach(request) => {
                                if matches!(mode, Refusal::Internal) {
                                    write_frame(
                                        &mut conn,
                                        &ControlReply::Error {
                                            kind: ControlErrorKind::Internal,
                                            message: "registry unavailable".into(),
                                        },
                                    )
                                    .await
                                    .unwrap();
                                    return;
                                }
                                write_frame(
                                    &mut conn,
                                    &ControlReply::AttachOk(Attached {
                                        proc_id: request.proc_id.clone(),
                                        running: matches!(mode, Refusal::Signal),
                                        cursor_head: 0,
                                        cursor_tail: 0,
                                        replay: vec![],
                                    }),
                                )
                                .await
                                .unwrap();
                                if matches!(mode, Refusal::Signal | Refusal::Exited) {
                                    write_frame(
                                        &mut conn,
                                        &ControlReply::Exited {
                                            proc_id: request.proc_id,
                                            status: Some(0),
                                            signalled: false,
                                            cursor: 0,
                                        },
                                    )
                                    .await
                                    .unwrap();
                                    return;
                                }
                                if matches!(mode, Refusal::Eof) {
                                    return;
                                }
                                if matches!(mode, Refusal::Protocol) {
                                    write_frame(&mut conn, &ControlReply::Ready).await.unwrap();
                                    return;
                                }
                                // No Exited: neither a false running bit nor a quiet connection proves exit.
                                tokio::time::sleep(Duration::from_secs(4)).await;
                                return;
                            }
                            ControlMsg::Signal(_) => {
                                write_frame(
                                    &mut conn,
                                    &ControlReply::Error {
                                        kind: ControlErrorKind::Internal,
                                        message: "signal refused".into(),
                                    },
                                )
                                .await
                                .unwrap();
                            }
                            other => panic!("deletion must only observe/stop, got {other:?}"),
                        }
                    }
                });
            }
        });
        Self {
            _dir: dir,
            sock,
            task,
        }
    }
}

#[tokio::test]
async fn legacy_disposal_protocol_and_signal_failures_retain_rows() {
    for mode in [
        Refusal::Internal,
        Refusal::OldVersion,
        Refusal::NoExited,
        Refusal::Eof,
        Refusal::Signal,
        Refusal::Protocol,
    ] {
        let owner = MockOwner::new(mode);
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let state = state_with_daemon(
            repo,
            DaemonClient {
                data_dir: owner._dir.path().into(),
                proc_supervisor_sock: Some(owner.sock.clone()),
            },
        );
        let (_, track, card, term) = seed(&state).await;
        state
            .repo
            .terminal_set_exit(&term, Some(2), false)
            .await
            .unwrap();
        assert_eq!(
            delete(&state, format!("/api/cards/{card}")).await.status(),
            StatusCode::CONFLICT,
            "{mode:?}"
        );
        assert!(
            state.repo.terminal_get(&term).await.unwrap().is_some(),
            "{mode:?}"
        );
        assert!(state.repo.track_get(&track).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn legacy_disposal_failed_commit_retains_reader_and_hook() {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo.clone());
    let (_, _, card, term) = seed(&state).await;
    let entry = owned_entry(&state, &owner.sock, &term).await;
    state
        .terminal_renderer
        .set_hook_settings_dir(owner._dir.path().into());
    let hook = owner._dir.path().join(format!("{card}.json"));
    std::fs::write(&hook, "retained").unwrap();
    repo.db().writer().execute(sqlx::query("CREATE TRIGGER reject_delete BEFORE DELETE ON cards BEGIN SELECT RAISE(ABORT,'fixture commit failure'); END")).await.unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        entry
            .wait_exit_persisted_for_test(Duration::from_secs(1))
            .await
    );
    assert!(state.terminal_renderer.get(&term).is_some());
    assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
    assert!(hook.exists());
}

#[tokio::test]
async fn legacy_disposal_exit_write_failure_retains_reader_and_rows() {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo.clone());
    let (_, _, card, term) = seed(&state).await;
    let entry = owned_entry(&state, &owner.sock, &term).await;
    repo.db().writer().execute(sqlx::query("CREATE TRIGGER reject_exit BEFORE UPDATE OF exit_code ON terminals BEGIN SELECT RAISE(ABORT,\
             'fixture exit failure'); END")).await.unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::CONFLICT
    );
    assert!(
        !entry
            .wait_exit_persisted_for_test(Duration::from_millis(20))
            .await
    );
    assert!(state.terminal_renderer.get(&term).is_some());
    assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
}

#[tokio::test]
async fn legacy_disposal_recorded_endpoint_mismatch_retains_rows() {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo.clone());
    let (_, track, card, term) = seed(&state).await;
    let output = json!({"target_type":"card","target_id":card,"result":{},"data":{"terminal_id":term,"terminal_launch":{"version":1,"state":"handed_off","terminal_id":term,"supervisor_sock":owner._dir.path().join("other.sock"),"pid":1}}});
    repo.db().writer().execute(sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_id,target_json,\
             payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) VALUES('legacy-owner',\
             'legacy-owner','terminal-worker','hash','card',?1,'{}','{}',?2,'succeeded',0,0)").bind(&card).bind(output.to_string())).await.unwrap();
    assert_eq!(
        delete(&state, format!("/api/tracks/{track}"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
}

#[tokio::test]
async fn legacy_disposal_missing_renderer_attaches_live_owner_without_relaunch() {
    use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
    use calm_session::{read_frame, write_frame};
    let owner = Owner::new();
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo);
    let (_, track, _, term) = seed(&state).await;
    let mut conn = calm_session::control::execution::connect_legacy(&owner.sock)
        .await
        .unwrap();
    write_frame(
        &mut conn,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: format!("term:{term}"),
            program: "/bin/sleep".into(),
            args: vec!["60".into()],
            envs: vec![],
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 8192,
        }),
    )
    .await
    .unwrap();
    let ControlReply::Spawned { pid } = read_frame::<ControlReply, _>(&mut conn).await.unwrap()
    else {
        panic!("owned spawn");
    };
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::Ready
    ));
    state.repo.terminal_set_pid(&term, Some(pid)).await.unwrap();
    state
        .repo
        .terminal_set_exit(&term, Some(2), false)
        .await
        .unwrap();
    assert!(state.terminal_renderer.get(&term).is_none());
    assert_eq!(
        delete(&state, format!("/api/tracks/{track}"))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let mut conn = calm_session::control::execution::connect_legacy(&owner.sock)
        .await
        .unwrap();
    write_frame(
        &mut conn,
        &ControlMsg::Attach(calm_session::control::AttachRequest {
            proc_id: format!("term:{term}"),
            from_cursor: None,
            reader_id: "fixture-observer".into(),
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::AttachOk(calm_session::control::Attached { running: false, .. })
    ));
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut conn).await.unwrap(),
        ControlReply::Exited { .. }
    ));
}

#[tokio::test]
async fn legacy_disposal_task_callback_failure_retains_rows() {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo.clone());
    let (_, track, card, term) = seed(&state).await;
    state
        .terminal_renderer
        .set_task_hook(calm_server::scheduler::TerminalTaskHook::new(
            repo.clone(),
            state.events.clone(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        ));
    let entry = owned_entry(&state, &owner.sock, &term).await;
    repo.db().writer().execute(sqlx::query("UPDATE cards SET payload=json_set(payload,'$.idempotency_key','legacy-hook') WHERE id=?1").bind(&card)).await.unwrap();
    repo.db().writer().execute(sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,\
             updated_at_ms) VALUES('legacy-hook',?1,'legacy-hook','terminal','true','null','running',?2,1,1)").bind(&track).bind(&card)).await.unwrap();
    repo.db().writer().execute(sqlx::query("CREATE TRIGGER reject_hook BEFORE UPDATE OF status ON tasks BEGIN SELECT RAISE(ABORT,'fixture callback failure'); END")).await.unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::CONFLICT
    );
    assert!(
        !entry
            .wait_exit_persisted_for_test(Duration::from_millis(20))
            .await
    );
    assert!(state.terminal_renderer.get(&term).is_some());
    assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
}

#[tokio::test]
async fn legacy_disposal_explicit_exited_uses_reader_persistence() {
    let owner = MockOwner::new(Refusal::Exited);
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = state_with_daemon(
        repo,
        DaemonClient {
            data_dir: owner._dir.path().into(),
            proc_supervisor_sock: Some(owner.sock.clone()),
        },
    );
    let (_, _, card, term) = seed(&state).await;
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(state.repo.terminal_get(&term).await.unwrap().is_none());
}
