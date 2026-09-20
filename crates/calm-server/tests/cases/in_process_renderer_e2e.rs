use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RouteRepo;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::model::{NewArea, NewCard, NewTerminal, NewTrack, Terminal};
use calm_server::routes::theme::RequestTheme;
use calm_server::terminal_renderer::{
    ClientPumpContext, RendererConfig, RendererEntry, TerminalRendererRegistry, run_client_pump,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding,
};
use serde_json::json;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

/// Liveness upper bound so a wedged test fails readably instead of hanging; not a contract.
/// Sized for CPU starvation on a saturated 2-core CI runner, not for expected latency — to
/// assert something happens *within* a deadline, measure elapsed time explicitly instead.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

#[tokio::test]
async fn in_process_renderer_drives_real_supervisor_and_pty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let registry = TerminalRendererRegistry::new();
    let terminal_id = Uuid::new_v4().to_string();
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 100,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                // The trailing sleep MUST outlast `LIVENESS_BUDGET`: otherwise a natural exit would satisfy
                // `wait_for_terminal_exited` and the test would pass with the input path cut entirely.
                "echo hello; printf '%085dWIDTH-MARKER\\n' 0; sleep 600".into(),
            ],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_hello_patch(&mut events).await;
    wait_for_child_ready(&mut events).await;

    let (client_tx, mut daemon_rx, pump) = spawn_client_pump(entry.clone());

    client_tx
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal_id.clone(),
            client_id: Uuid::new_v4(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .expect("send client hello");
    let hello = timeout(LIVENESS_BUDGET, daemon_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    let DaemonMsg::ServerHello { snapshot, .. } = hello else {
        panic!("expected ServerHello");
    };
    assert_eq!((snapshot.cols, snapshot.rows), (100, 24));
    assert!(
        String::from_utf8_lossy(&snapshot.data).contains("WIDTH-MARKER"),
        "80-col remount desire must not clip marker from current 100-col model"
    );

    client_tx
        .send(ClientMsg::Input {
            data: b"\x03".to_vec(),
            input_seq: 1,
        })
        .await
        .expect("send ctrl-c");
    wait_for_input_ack(&mut daemon_rx, 1).await;
    wait_for_terminal_exited(&mut events).await;
    wait_for_daemon_terminal_exited(&mut daemon_rx).await;
    assert!(
        timeout(LIVENESS_BUDGET, entry.wait_exited())
            .await
            .expect("entry exit timeout")
            .is_some(),
        "renderer entry did not surface supervisor Exited"
    );

    registry.drop_entry(&terminal_id).await;
    assert!(registry.get(&terminal_id).is_none());

    drop(client_tx);
    timeout(LIVENESS_BUDGET, pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

#[tokio::test]
async fn late_client_attach_receives_sticky_terminal_exited() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let registry = TerminalRendererRegistry::new();
    let terminal_id = Uuid::new_v4().to_string();
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo late; exit 7".into()],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_terminal_exited(&mut events).await;

    let (client_tx, mut daemon_rx, pump) = spawn_client_pump(entry.clone());
    send_client_hello(&client_tx, &terminal_id).await;
    let hello = timeout(LIVENESS_BUDGET, daemon_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    assert!(
        matches!(hello, DaemonMsg::ServerHello { .. }),
        "expected ServerHello, got {hello:?}"
    );
    let exit = wait_for_daemon_terminal_exited(&mut daemon_rx).await;
    assert_eq!(exit.code, Some(7));

    registry.drop_entry(&terminal_id).await;
    drop(client_tx);
    timeout(LIVENESS_BUDGET, pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

#[tokio::test]
async fn registry_ensure_lazily_reattaches_when_registry_is_empty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let terminal_id = Uuid::new_v4().to_string();
    let cfg = RendererConfig {
        terminal_id: terminal_id.clone(),
        cols: 80,
        rows: 24,
        buffer_bytes: 1 << 20,
        terminal_fg: (216, 219, 226),
        terminal_bg: (15, 20, 24),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "cat".into()],
        envs: std::env::vars().collect(),
        cwd: workspace_root().display().to_string(),
        supervisor_sock: control_sock.clone(),
    };

    let registry_a = TerminalRendererRegistry::new();
    let entry_a = registry_a
        .ensure(cfg.clone())
        .await
        .expect("ensure initial renderer");
    assert!(
        registry_a.get(&terminal_id).is_some(),
        "initial registry should contain renderer entry"
    );
    drop(entry_a);
    drop(registry_a);

    let registry_b = TerminalRendererRegistry::new();
    let entry_b = registry_b
        .ensure(cfg)
        .await
        .expect("reattach renderer after registry drop");
    assert!(
        registry_b.get(&terminal_id).is_some(),
        "fresh registry should contain reattached renderer entry"
    );

    let mut events = entry_b.subscribe();
    assert_no_terminal_exited(&mut events, Duration::from_secs(1)).await;

    let (client_tx, mut daemon_rx, pump) = spawn_client_pump(entry_b.clone());
    send_client_hello(&client_tx, &terminal_id).await;
    let hello = timeout(LIVENESS_BUDGET, daemon_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    assert!(
        matches!(hello, DaemonMsg::ServerHello { .. }),
        "expected ServerHello, got {hello:?}"
    );

    client_tx
        .send(ClientMsg::Input {
            data: b"lazy-reattach\n".to_vec(),
            input_seq: 7,
        })
        .await
        .expect("send input through reattached renderer");
    wait_for_input_ack(&mut daemon_rx, 7).await;

    registry_b.drop_entry(&terminal_id).await;
    drop(client_tx);
    timeout(LIVENESS_BUDGET, pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

fn spawn_client_pump(
    entry: Arc<RendererEntry>,
) -> (
    mpsc::Sender<ClientMsg>,
    mpsc::Receiver<DaemonMsg>,
    JoinHandle<anyhow::Result<()>>,
) {
    let (client_tx, client_rx) = mpsc::channel::<ClientMsg>(16);
    let (daemon_tx, daemon_rx) = mpsc::channel::<DaemonMsg>(64);
    let pump = tokio::spawn(async move {
        run_client_pump(
            client_rx,
            daemon_tx,
            ClientPumpContext {
                input_barrier: entry.handle.input_barrier.clone(),
                input_scope: calm_server::terminal_renderer::ClientInputScope::InteractiveUser,
                event_rx: entry.handle.event_tx.subscribe(),
                event_tx: entry.handle.event_tx.clone(),
                render_plane: entry.handle.render_plane.clone(),
                exit: entry.exit.clone(),
                supervisor_tx: entry.handle.supervisor_tx.clone(),
                owner_registry: entry.handle.owner_registry.clone(),
                session_id: entry.handle.session_id,
                terminal_id: entry.terminal_id.clone(),
            },
        )
        .await
    });
    (client_tx, daemon_rx, pump)
}

async fn send_client_hello(client_tx: &mpsc::Sender<ClientMsg>, terminal_id: &str) {
    client_tx
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal_id.to_string(),
            client_id: Uuid::new_v4(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .expect("send client hello");
}

async fn wait_for_hello_patch(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for hello patch");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("hello patch timeout")
            .expect("event channel closed");
        if let DaemonMsg::RenderPatch(patch) = msg
            && String::from_utf8_lossy(&patch.data).contains("hello")
        {
            return;
        }
    }
}

async fn wait_for_child_ready(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for ChildReady");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("child ready timeout")
            .expect("event channel closed");
        if matches!(msg, DaemonMsg::ChildReady { .. }) {
            return;
        }
    }
}

async fn wait_for_input_ack(rx: &mut mpsc::Receiver<DaemonMsg>, expected: u64) {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for InputAck");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("input ack timeout")
            .expect("daemon channel closed");
        if let DaemonMsg::InputAck { input_seq } = msg {
            assert_eq!(input_seq, expected);
            return;
        }
    }
}

async fn wait_for_terminal_exited(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for TerminalExited");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("terminal exited timeout")
            .expect("event channel closed");
        if matches!(msg, DaemonMsg::TerminalExited { .. }) {
            return;
        }
    }
}

async fn assert_no_terminal_exited(
    rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>,
    duration: Duration,
) {
    let deadline = tokio::time::Instant::now() + duration;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(DaemonMsg::TerminalExited { .. })) => {
                panic!("reattached live renderer unexpectedly emitted TerminalExited");
            }
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("reattached renderer event channel closed");
            }
            Err(_) => return,
        }
    }
}

struct ExitFrame {
    code: Option<i32>,
}

async fn wait_for_daemon_terminal_exited(rx: &mut mpsc::Receiver<DaemonMsg>) -> ExitFrame {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for daemon TerminalExited"
        );
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("daemon terminal exited timeout")
            .expect("daemon channel closed");
        if let DaemonMsg::TerminalExited { code, .. } = msg {
            return ExitFrame { code };
        }
    }
}

/// `drop_entry` must not abort the supervisor attach reader before it has observed `Exited`,
/// since that reader is what persists the exit. The child ignores SIGTERM and leaves a `setsid`'d
/// grandchild holding the pty slave, so `Exited` lands strictly *after* the kill; the assertion
/// is on the persisted row, not on the in-memory `entry.exit`.
#[tokio::test]
async fn drop_entry_persists_the_terminal_exit_to_the_database() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let term = seed_terminal_row(repo.as_ref()).await;
    let terminal_id = term.id.clone();

    let registry = TerminalRendererRegistry::new_with_repo(route_repo);
    // The grandchild records its own pid so the self-check below can prove it outlived the
    // teardown; `setsid` keeps it out of reach of the process-group kill.
    let gc_pid_file = temp.path().join("grandchild.pid");
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                // The grandchild's sleep must outlast the whole test: the self-check asserts it is *still
                // alive* after teardown.
                format!(
                    "trap '' TERM; setsid sh -c 'echo $$ > {}; sleep 600' & echo up; sleep 30",
                    gc_pid_file.display()
                ),
            ],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_child_ready(&mut events).await;
    assert!(
        entry.exit.lock().expect("exit mutex").is_none(),
        "sanity: the child must still be running before teardown"
    );
    assert!(
        repo.terminal_get(&terminal_id)
            .await
            .expect("terminal_get")
            .expect("terminal row")
            .exit_code
            .is_none(),
        "sanity: the row must carry no exit before teardown"
    );
    // Written by the grandchild itself, so it is its real pid whatever `setsid` decides about forking.
    let grandchild_pid = wait_for_pid_file(&gc_pid_file).await;

    registry.drop_entry(&terminal_id).await;

    // Self-check: the pty slave holder outlived the teardown, so the master could not EOF and the
    // supervisor really took the drain-grace path.
    assert!(
        process_is_alive(grandchild_pid),
        "this test must exercise the DEGRADED path, but the setsid'd grandchild (pid \
         {grandchild_pid}) was already gone after teardown — the pty master EOFed, so \
         `Exited` never had to wait for the drain grace"
    );

    // The load-bearing assertion: the exit reached the database.
    let row = repo
        .terminal_get(&terminal_id)
        .await
        .expect("terminal_get")
        .expect("terminal row");
    assert!(
        row.signal_killed,
        "issue #993 R1: terminal exit was not persisted (signal_killed still false) — \
         drop_entry cut the attach reader off before `terminal_set_exit` ran; row = {row:?}"
    );
    assert!(
        row.pty_output.contains("up"),
        "the attach reader must persist the real merged PTY output; row = {row:?}"
    );
    assert!(
        !row.pty_output_truncated,
        "the tiny fixture output fits the durable evidence cap"
    );
    // Cheap locator: `entry.exit` is stamped inside the same `Exited` arm, before the persistence calls.
    assert!(
        entry.exit.lock().expect("exit mutex").is_some(),
        "issue #993 R1: drop_entry aborted the supervisor attach reader before it saw \
         Exited"
    );

    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

/// `TERM_TO_KILL_GRACE` in `terminal_renderer` — private there, mirrored here.
const TERM_TO_KILL_GRACE: Duration = Duration::from_millis(200);

/// Teardown must key on the exit being *persisted*, not on the attach reader's task handle:
/// the reader also exits on a stream error having written nothing. Kill the supervisor under a
/// live renderer, then assert `drop_entry` still takes at least `TERM_TO_KILL_GRACE` (only ever
/// compared in the safe direction, so a loaded box cannot flake it).
#[tokio::test]
async fn drop_entry_keeps_the_term_grace_when_the_attach_reader_died_early() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let term = seed_terminal_row(repo.as_ref()).await;
    let terminal_id = term.id.clone();

    let registry = TerminalRendererRegistry::new_with_repo(route_repo);
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            // Short-lived: killing the supervisor orphans this child, so it must reap itself.
            args: vec!["-c".into(), "echo up; sleep 3".into()],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_child_ready(&mut events).await;

    // Break the attach stream: the reader's next `read_frame` errors out having persisted nothing.
    supervisor.kill().await.expect("kill supervisor");
    let _ = supervisor.wait().await;
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    while UnixStream::connect(&control_sock).await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor kept accepting connections after being killed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Only has to cover the wakeup so the teardown really faces a *finished* reader.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = std::time::Instant::now();
    registry.drop_entry(&terminal_id).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed >= TERM_TO_KILL_GRACE,
        "issue #993 R3-A: drop_entry escalated to SIGKILL after only {elapsed:?} — it \
         treated the attach reader's *death* as a persisted exit, so the child's \
         {TERM_TO_KILL_GRACE:?} SIGTERM grace disappeared",
    );

    // Self-check: nothing was persisted, i.e. this really is the degraded path.
    let row = repo
        .terminal_get(&terminal_id)
        .await
        .expect("terminal_get")
        .expect("terminal row");
    assert!(
        !row.signal_killed && row.exit_code.is_none(),
        "this test must exercise the DEGRADED path: the attach reader was supposed to die \
         before persisting anything, but the row already carries an exit; row = {row:?}"
    );
    assert!(
        entry.exit.lock().expect("exit mutex").is_none(),
        "sanity: the attach reader never saw Exited on a dead stream"
    );
}

/// The TERM→KILL grace is cut short by the *leader's* exit, so the unconditional group SIGKILL
/// is the only thing between a stubborn group member and a leak. The leader dies on SIGTERM
/// immediately while a member ignores TERM and HUP; after teardown the member must be gone.
#[tokio::test]
async fn drop_entry_kills_process_group_members_that_outlive_the_leader() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let term = seed_terminal_row(repo.as_ref()).await;
    let terminal_id = term.id.clone();

    let registry = TerminalRendererRegistry::new_with_repo(route_repo);
    let member_pid_file = temp.path().join("group-member.pid");
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!(
                    // No `setsid`: the member stays in the leader's process group. `exec` makes the `sleep` itself
                    // the leader. The member's sleep MUST outlast the poll deadline below, or a leaked member would
                    // be indistinguishable from one that finished on its own.
                    "sh -c 'trap \"\" TERM HUP; echo $$ > {}; sleep 600' & echo up; exec sleep 30",
                    member_pid_file.display()
                ),
            ],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_child_ready(&mut events).await;
    let member_pid = wait_for_pid_file(&member_pid_file).await;
    assert!(
        process_is_alive(member_pid),
        "sanity: the group member (pid {member_pid}) must be running before teardown"
    );

    registry.drop_entry(&terminal_id).await;

    // Only the group SIGKILL can end the member; polling covers signal delivery and reaping.
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    while process_is_alive(member_pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "issue #993 R3-B: the process-group member (pid {member_pid}) survived \
             drop_entry — cutting the TERM grace short at the leader's exit must not \
             cost the unconditional SIGKILL that reaps the rest of the group"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Self-check: the leader's exit really was persisted — the event that ends the grace early.
    let row = repo
        .terminal_get(&terminal_id)
        .await
        .expect("terminal_get")
        .expect("terminal row");
    assert!(
        row.signal_killed,
        "the leader's signalled exit must be persisted by the attach reader; row = {row:?}"
    );

    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

/// Minimal area → track → card → terminal chain so `terminal_set_exit` has a row to write to.
pub(super) async fn seed_terminal_row(repo: &SqlxRepo) -> Terminal {
    let area = repo
        .area_create(NewArea {
            name: "renderer-e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "renderer-e2e".into(),
            sort: None,
            cwd: workspace_root().display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    let card = repo
        .card_create(NewCard {
            track_id: track.id,
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .expect("create card");
    repo.terminal_create(NewTerminal {
        card_id: card.id,
        program: "sleep 30".into(),
        cwd: workspace_root().display().to_string(),
        env: json!({}),
        theme: RequestTheme::default_dark(),
    })
    .await
    .expect("create terminal")
}

/// Waits for the grandchild to publish its pid, then parses it.
pub(super) async fn wait_for_pid_file(path: &Path) -> i32 {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<i32>()
        {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the setsid'd grandchild never wrote its pid to {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// True when `pid` names a live (non-zombie) process; a zombie has already closed every fd it held.
pub(super) fn process_is_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `comm` may contain spaces and parens, so split at the LAST ')'.
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    matches!(rest.split_whitespace().next(), Some(state) if state != "Z" && state != "X")
}

pub(super) async fn spawn_proc_supervisor(control_sock: &Path) -> Child {
    let child = Command::new(locate_bin("calm-proc-supervisor"))
        .arg("--control-sock")
        .arg(control_sock)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn supervisor");
    wait_until_listening(control_sock).await;
    assert!(child.id().is_some(), "supervisor exited before listening");
    child
}

async fn wait_until_listening(sock: &Path) {
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if UnixStream::connect(sock).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("supervisor never listened on {}", sock.display());
}

fn locate_bin(name: &str) -> PathBuf {
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(env_key) {
        return PathBuf::from(path);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile = me
        .parent()
        .and_then(|p| p.parent())
        .expect("test bin parent");
    let candidate = target_profile.join(name);
    if candidate.exists() {
        return candidate;
    }
    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "calm-proc-supervisor",
            "--bin",
            "calm-proc-supervisor",
            "--locked",
        ])
        .status()
        .expect("run cargo build for calm-proc-supervisor");
    assert!(
        status.success(),
        "cargo build for calm-proc-supervisor failed with {status}"
    );
    if candidate.exists() {
        return candidate;
    }
    panic!("{name} binary not found at {}", candidate.display());
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}
