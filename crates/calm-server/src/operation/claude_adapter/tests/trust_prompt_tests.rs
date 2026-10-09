//! #1755: the kernel accepts Claude's workspace trust dialog in a scheduler-spawned worker's PTY,
//! and only there. Real PTYs through the production spawn path; the fake `claude` paints the
//! dialog and logs every byte it reads.
#![cfg(feature = "fixtures")]
use super::*;
use crate::operation::claude_adapter::trust_prompt::{TrustOutcome, TrustPromptWatch};
use crate::operation::claude_restart_adapter::{
    ClaudeRestartAdapter, ClaudeRestartOperationPayload,
};
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const DOWN: &[u8] = b"\x1b[B";

/// A private copy of the fake `claude` with its scenario.
struct FakeClaude {
    dir: tempfile::TempDir,
}
impl FakeClaude {
    fn new(scenario: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_trust_dialog/claude");
        std::fs::copy(source, dir.path().join("claude")).unwrap();
        std::fs::write(dir.path().join("scenario"), scenario).unwrap();
        Self { dir }
    }
    fn codex(&self) -> Arc<CodexClient> {
        let mut codex = CodexClient::new_stub();
        codex.claude_bin = self
            .dir
            .path()
            .join("claude")
            .to_string_lossy()
            .into_owned();
        Arc::new(codex)
    }
    fn has(&self, name: &str) -> bool {
        self.dir.path().join(name).exists()
    }
    fn received(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("received")).unwrap_or_default()
    }
    async fn wait_for(&self, name: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !self.has(name) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the fake claude never wrote {name}"));
    }
}

/// The production spawn context over a real proc supervisor and renderer. A renderer without a
/// repository spawns a terminal unbound to any launch record.
struct Pty {
    _supervisor: InProcessProcSupervisor,
    renderer: Arc<TerminalRendererRegistry>,
    ctx: SpawnCtx,
    _mcp_dir: tempfile::TempDir,
    mcp_server: Arc<McpServer>,
}
async fn pty(harness: &ClaudeWorkerHarness, renderer: Arc<TerminalRendererRegistry>) -> Pty {
    let supervisor = InProcessProcSupervisor::start().await.unwrap();
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(supervisor.sock().into());
    let ctx = SpawnCtx::new(
        harness.repo.clone(),
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone())),
        Arc::new(daemon),
        renderer.clone(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let mcp_dir = tempfile::tempdir().unwrap();
    let mcp_server = McpServer::new_for_test(crate::mcp_server::McpShimConfig {
        shim_bin: mcp_dir.path().join("neige-mcp-stdio-shim"),
        socket_path: mcp_dir.path().join("kernel.sock"),
    });
    Pty {
        _supervisor: supervisor,
        renderer,
        ctx,
        _mcp_dir: mcp_dir,
        mcp_server,
    }
}

fn watch(appear_ms: u64) -> (TrustPromptWatch, UnboundedReceiver<TrustOutcome>) {
    let (outcomes, received) = unbounded_channel();
    let watch = TrustPromptWatch {
        appear: Duration::from_millis(appear_ms),
        step: Duration::from_secs(2),
        outcomes: Some(outcomes),
    };
    (watch, received)
}

async fn outcome(outcomes: &mut UnboundedReceiver<TrustOutcome>) -> TrustOutcome {
    tokio::time::timeout(Duration::from_secs(30), outcomes.recv())
        .await
        .expect("the trust watch never ended")
        .expect("the trust watch was dropped")
}

/// A worker spawned by the scheduler's adapter with the fake `claude`.
struct Worker {
    harness: ClaudeWorkerHarness,
    pty: Pty,
    card_id: String,
    terminal_id: String,
    task_id: String,
}
/// Submitted to the operation runtime as the scheduler submits a task's worker.
async fn spawn_worker(fake: &FakeClaude, watch: TrustPromptWatch) -> Worker {
    let harness = claude_worker_harness().await;
    let pty = pty(
        &harness,
        TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
    )
    .await;
    let task_id = format!("{}:trust", harness.track_id);
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'trust', 'claude', 'work', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(&task_id)
    .bind(&harness.track_id)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let adapter = ClaudeWorkerAdapter::new(
        harness.repo.clone(),
        fake.codex(),
        Some(pty.mcp_server.clone()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        harness.workspace.path().to_path_buf(),
    )
    .with_trust_prompt(watch);
    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let runtime = crate::operation::OperationRuntime::new(
        op_repo.clone(),
        vec![Arc::new(adapter)],
        harness.events.clone(),
        OperationCompletionBus::new(),
        pty.ctx.clone(),
    )
    .await
    .unwrap();
    let payload = claude_worker_payload(&harness.track_id, "trust");
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(task_id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let op_id = runtime
        .submit("claude-worker", key, payload)
        .await
        .expect("worker submit");
    let op = op_repo.get_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(
        op.phase,
        crate::operation::Phase::Succeeded,
        "{:?}",
        op.last_error
    );
    let output = op.tx_output.unwrap();
    Worker {
        card_id: output.output_string("card_id", "test").unwrap(),
        terminal_id: output.output_string("terminal_id", "test").unwrap(),
        task_id,
        harness,
        pty,
    }
}
impl Worker {
    async fn task(&self) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, status_detail FROM tasks WHERE id = ?1")
            .bind(&self.task_id)
            .fetch_one(self.harness.repo.pool())
            .await
            .unwrap()
    }
    async fn stop(self) {
        self.pty.renderer.drop_entry(&self.terminal_id).await;
        release_workspace_lease_for_card_repo(
            self.harness.repo.as_ref(),
            &self.harness.events,
            &self.card_id,
            ReleaseDelivery::Commit(AttemptOutcome::Completed),
        )
        .await
        .ok();
    }
}

/// A human client that has claimed the terminal; aborting the pump releases the lease.
async fn human_claims(
    entry: &Arc<crate::terminal_renderer::RendererEntry>,
) -> tokio::task::AbortHandle {
    use crate::terminal_renderer::{ClientInputScope, ClientPumpContext, run_client_pump};
    use calm_session::{
        ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
        RenderEncoding,
    };
    let user = uuid::Uuid::new_v4();
    let (incoming, rx) = tokio::sync::mpsc::channel(8);
    let (tx, mut outgoing) = tokio::sync::mpsc::channel(32);
    let pump = tokio::spawn(run_client_pump(
        rx,
        tx,
        ClientPumpContext {
            input_barrier: entry.handle.input_barrier.clone(),
            input_scope: ClientInputScope::InteractiveUser,
            event_rx: entry.subscribe(),
            event_tx: entry.handle.event_tx.clone(),
            render_plane: entry.handle.render_plane.clone(),
            exit: entry.exit.clone(),
            supervisor_tx: entry.handle.supervisor_tx.clone(),
            owner_registry: entry.handle.owner_registry.clone(),
            session_id: entry.handle.session_id,
            terminal_id: entry.terminal_id.clone(),
        },
    ));
    incoming
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: entry.terminal_id.clone(),
            client_id: user,
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
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .unwrap();
    incoming.send(ClientMsg::OwnerClaim).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            outgoing.recv().await,
            Some(DaemonMsg::OwnerChanged { owner_client_id: Some(id) }) if id == user
        ) {}
    })
    .await
    .expect("the human claim was never granted");
    // Keep the human's connection open until the pump is aborted.
    tokio::spawn(async move {
        let _incoming = incoming;
        while outgoing.recv().await.is_some() {}
    });
    pump.abort_handle()
}

#[tokio::test]
async fn worker_spawn_accepts_the_trust_dialog_with_down_then_enter() {
    let fake = FakeClaude::new("dialog");
    let (watch, mut outcomes) = watch(15_000);
    let worker = spawn_worker(&fake, watch).await;

    assert_eq!(outcome(&mut outcomes).await, TrustOutcome::Accepted);
    assert_eq!(fake.received(), [DOWN, b"\r"].concat(), "Down, then Enter");
    assert!(fake.has("accepted") && !fake.has("declined"));
    assert_eq!(
        worker.task().await.0,
        "dispatched",
        "an accepted dialog fails nothing"
    );
    worker.stop().await;
}

#[tokio::test]
async fn worker_spawn_without_a_dialog_sends_nothing() {
    let fake = FakeClaude::new("none");
    let (watch, mut outcomes) = watch(1_500);
    let worker = spawn_worker(&fake, watch).await;

    assert_eq!(outcome(&mut outcomes).await, TrustOutcome::NotShown);
    assert_eq!(fake.received(), b"");
    assert_eq!(
        worker.task().await.0,
        "dispatched",
        "an unrecognized screen is no failure"
    );
    worker.stop().await;
}

#[tokio::test]
async fn worker_terminal_held_by_a_human_gets_no_input() {
    let fake = FakeClaude::new("gated");
    let (watch, mut outcomes) = watch(15_000);
    let worker = spawn_worker(&fake, watch).await;
    let entry = worker.pty.renderer.get(&worker.terminal_id).unwrap();
    let human = human_claims(&entry).await;
    std::fs::write(fake.dir.path().join("go"), "").unwrap();

    assert_eq!(outcome(&mut outcomes).await, TrustOutcome::HumanOwned);
    assert!(fake.has("painted"));
    assert_eq!(fake.received(), b"", "the human answers, not the kernel");
    assert_eq!(worker.task().await.0, "dispatched");
    human.abort();
    worker.stop().await;
}

#[tokio::test]
async fn worker_dialog_whose_cursor_never_reaches_yes_fails_the_task() {
    let fake = FakeClaude::new("stuck");
    let (watch, mut outcomes) = watch(15_000);
    let worker = spawn_worker(&fake, watch).await;

    let TrustOutcome::NotAccepted(reason) = outcome(&mut outcomes).await else {
        panic!("a stuck cursor is not accepted");
    };
    assert_eq!(
        fake.received(),
        DOWN,
        "never Enter while the cursor is on No"
    );
    assert!(!fake.has("declined"));
    let (status, detail) = worker.task().await;
    assert_eq!(status, "failed");
    assert_eq!(
        detail.as_deref(),
        Some(format!("{}: {reason}", crate::scheduler::WORKER_STARTUP_BLOCKED).as_str())
    );
    assert!(
        reason.contains("trust dialog") && reason.contains("Yes, I trust this folder"),
        "{reason}"
    );
    let marker: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(handle_state_json, '$.timeout_cleanup.reason') FROM worker_sessions \
         WHERE card_id = ?1",
    )
    .bind(&worker.card_id)
    .fetch_one(worker.harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        marker.as_deref(),
        Some("worker_startup_blocked"),
        "the blocked worker is marked for the sweep's reap"
    );
    worker.stop().await;
}

#[tokio::test]
async fn owner_claude_card_spawn_gets_no_input() {
    let fake = FakeClaude::new("dialog");
    let harness = claude_worker_harness().await;
    let pty = pty(
        &harness,
        TerminalRendererRegistry::new_with_repo(harness.repo.clone()),
    )
    .await;
    let codex = fake.codex();
    let request = prepare_claude_create_request(
        harness.repo.as_ref(),
        &codex,
        normalize_claude_create_request(ClaudeCreateRequestInput {
            track_id: harness.track_id.clone(),
            title: None,
            sort: None,
            cwd: Some(harness.worktree.to_string_lossy().into_owned()),
            prompt: None,
            icon_bg: None,
            icon_fg: None,
            theme: RequestTheme::default_dark(),
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let payload = serde_json::to_value(ClaudeCreateOperationPayload {
        actor: ActorId::User,
        worker_session_id: None,
        request,
    })
    .unwrap();
    let adapter = ClaudeAdapter::new(
        harness.repo.clone(),
        codex,
        CardRoleCache::new(),
        TrackAreaCache::new(),
    );
    let mut op = claude_worker_op("op-owner", payload.clone());
    op.kind = "claude-create".into();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    adapter
        .spawn_side_effect(&output, &op, &pty.ctx)
        .await
        .expect("owner spawn");

    fake.wait_for("painted").await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(fake.received(), b"", "an owner's dialog is the owner's");
    pty.renderer
        .drop_entry(&output.output_string("terminal_id", "test").unwrap())
        .await;
}

#[tokio::test]
async fn worker_card_restart_accepts_the_trust_dialog() {
    let fake = FakeClaude::new("dialog");
    let mut harness = claude_worker_harness().await;
    // A prepared worker card's own launch record admits only an attach, so the restarted Claude
    // is spawned unbound to it.
    let pty = pty(&harness, TerminalRendererRegistry::new()).await;
    harness.adapter = ClaudeWorkerAdapter::new(
        harness.repo.clone(),
        fake.codex(),
        Some(pty.mcp_server.clone()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        harness.workspace.path().to_path_buf(),
    );
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'again', 'claude', 'work', 'null', '[]', 'running', 1, 1)",
    )
    .bind(format!("{}:again", harness.track_id))
    .bind(&harness.track_id)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let first = prepare_claude_worker_as_scheduled(&harness, "again").await;
    let card_id = first.output_string("card_id", "test").unwrap();
    let terminal_id = first.output_string("terminal_id", "test").unwrap();
    crate::db::RepoOutOfDomain::terminal_set_exit(
        harness.repo.as_ref(),
        &terminal_id,
        Some(0),
        false,
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    crate::db::sqlite::session_complete_for_card_tx(&mut tx, &card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (watch, mut outcomes) = watch(15_000);
    let restart = ClaudeRestartAdapter::new(
        harness.repo.clone(),
        fake.codex(),
        Some(pty.mcp_server.clone()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
    )
    .with_trust_prompt(watch);
    let payload = serde_json::to_value(ClaudeRestartOperationPayload {
        actor: ActorId::User,
        worker_session_id: None,
        card_id: card_id.clone(),
    })
    .unwrap();
    let op = claude_worker_op("op-restart", payload.clone());
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let output = restart.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    restart
        .spawn_side_effect(&output, &op, &pty.ctx)
        .await
        .expect("restart spawn");

    assert_eq!(outcome(&mut outcomes).await, TrustOutcome::Accepted);
    assert_eq!(fake.received(), [DOWN, b"\r"].concat(), "Down, then Enter");
    assert!(fake.has("accepted"));
    pty.renderer.drop_entry(&terminal_id).await;
    release_workspace_lease_for_card_repo(
        harness.repo.as_ref(),
        &harness.events,
        &card_id,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .ok();
}
