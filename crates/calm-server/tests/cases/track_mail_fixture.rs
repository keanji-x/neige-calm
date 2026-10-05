//! #2130 mail world: one Area whose Tracks each have a Codex Planner with a live harness on one
//! fake shared app-server, the real Dispatcher (so a mail's `track.wake_requested` is pushed to
//! the recipient's harness) and the real MCP listener (so every call goes through the transport and
//! the `neige` forwarder protocol). Tracks work in private temp directories, never a checkout.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, session_mcp_token_set_tx, session_start_runtime_tx,
};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, HarnessState, Observation,
    PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::{AppContext, ToolCallIdentity};
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::TrackReportPayload;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::mcp::{call_tool_card_bound, cli_output, neige_cli_via_socket};

const BUDGET: Duration = Duration::from_secs(5);

/// One Track and its Codex Planner.
pub struct Planner {
    pub track_id: TrackId,
    pub card_id: CardId,
    pub session_id: String,
    pub thread_id: String,
    pub token: String,
    pub harness: Option<PlannerHarness>,
}

impl Planner {
    pub fn harness(&self) -> &PlannerHarness {
        self.harness
            .as_ref()
            .expect("this Planner has a live harness")
    }

    /// The identity the transport resolves for this Planner's token.
    pub fn identity(&self, area_id: &AreaId) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: self.card_id.to_string(),
            role: CardRole::Planner,
            provider: AgentProvider::Codex,
            session_id: self.session_id.clone(),
            track_id: Some(self.track_id.to_string()),
            area_id: area_id.to_string(),
            thread_id: "card-bound".into(),
        }
    }
}

pub struct World {
    pub repo: Arc<SqlxRepo>,
    pub repo_dyn: Arc<dyn Repo>,
    pub events: EventBus,
    pub role_cache: CardRoleCache,
    pub area_cache: TrackAreaCache,
    pub daemon: Arc<SharedCodexAppServer>,
    pub registry: HarnessRegistry,
    pub area_id: AreaId,
    pub planners: Vec<Planner>,
    pub socket: PathBuf,
    _dispatcher: Dispatcher,
    _mcp: Arc<McpServer>,
    _sockets: TempDir,
    pub dir: TempDir,
}

impl World {
    /// One Area with one live Codex Planner per title, in order.
    pub async fn new(titles: &[&str]) -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("sqlite"));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let area = repo_dyn
            .area_create(NewArea {
                name: "mail".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let events = EventBus::new();
        let role_cache = CardRoleCache::new();
        let area_cache = TrackAreaCache::new();
        let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo_dyn.clone(), None);
        let registry = HarnessRegistry::new();
        let sockets = calm_test_sockets::socket_dir("mail");
        let socket = calm_test_sockets::socket_path(sockets.path(), "kernel.sock");
        let write = WriteContext::new(role_cache.clone(), area_cache.clone());
        let mcp = McpServer::spawn(
            repo_dyn.clone(),
            events.clone(),
            write.clone(),
            socket.clone(),
            PathBuf::from("/nonexistent-shim-bin"),
            build_default_registry(),
            None,
            Arc::new(tokio::sync::OnceCell::new()),
            Arc::new(tokio::sync::OnceCell::new()),
            dir.path().join("gate-logs"),
        )
        .await
        .expect("spawn McpServer");
        let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo_dyn.clone();
        let dispatcher = Dispatcher::spawn_with_terminal_renderer_and_harness(
            repo_dyn.clone(),
            events.clone(),
            write,
            Arc::new(CodexClient::new_stub()),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(route_repo),
            None,
            registry.clone(),
            daemon.clone(),
            dir.path().join("workspaces"),
            4,
        );
        let mut world = Self {
            repo,
            repo_dyn,
            events,
            role_cache,
            area_cache,
            daemon,
            registry,
            area_id: area.id,
            planners: Vec::new(),
            socket,
            _dispatcher: dispatcher,
            _mcp: mcp,
            _sockets: sockets,
            dir,
        };
        for title in titles {
            let planner = world.add_planner(&world.area_id.clone(), title).await;
            world.planners.push(planner);
        }
        world
    }

    pub fn p(&self, index: usize) -> &Planner {
        &self.planners[index]
    }

    /// A Track in `area` working in a private directory.
    pub async fn add_track(&self, area: &AreaId, title: &str) -> TrackId {
        let cwd = self.dir.path().join(new_id());
        std::fs::create_dir_all(&cwd).unwrap();
        let track = self
            .repo_dyn
            .track_create(NewTrack {
                template_input: None,
                area_id: area.clone(),
                title: title.into(),
                sort: None,
                cwd: cwd.display().to_string(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        self.area_cache.insert(track.id.clone(), area.clone());
        // Every Track has its report card, which is what `neige_area_ls` lists.
        let mut tx = self.repo.pool().begin().await.unwrap();
        card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
            },
            CardRole::ReportCard,
            false,
            &self.role_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        track.id
    }

    /// A Track with a Planner card, its idle shared-planner session (MCP token minted) and a live harness.
    pub async fn add_planner(&self, area: &AreaId, title: &str) -> Planner {
        let mut planner = self.add_planner_without_harness(area, title).await;
        let harness = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: planner.session_id.clone(),
            track_id: planner.track_id.clone(),
            card_id: planner.card_id.clone(),
            thread_id: Some(planner.thread_id.clone()),
            repo: self.repo_dyn.clone(),
            events: self.events.clone(),
            card_role_cache: self.role_cache.clone(),
            track_area_cache: self.area_cache.clone(),
            backend: self.daemon.clone().into(),
            live_replies: calm_server::harness::LiveReplies::for_test(),
            config: HarnessConfig::default(),
            snapshot: idle_snapshot(&planner.thread_id),
        });
        self.registry
            .insert(planner.session_id.clone(), harness.clone());
        planner.harness = Some(harness);
        planner
    }

    /// The same Planner with no harness running: a down Planner whose next start replays.
    pub async fn add_planner_without_harness(&self, area: &AreaId, title: &str) -> Planner {
        let track_id = self.add_track(area, title).await;
        let card = self
            .repo_dyn
            .card_create(NewCard {
                track_id: track_id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1, "planner_harness": true, "planner_provider": "codex"}),
            })
            .await
            .unwrap();
        crate::support::mcp::set_persisted_card_role(
            self.repo_dyn.as_ref(),
            card.id.as_str(),
            CardRole::Planner,
        )
        .await;
        self.role_cache
            .insert(card.id.clone(), CardRole::Planner, track_id.clone());
        let session_id = new_id();
        let thread_id = format!("thread-{session_id}");
        let token = calm_server::mcp_server::auth::CardMcpToken::generate();
        let mut tx = self.repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: session_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some(thread_id.clone()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(idle_snapshot(&thread_id)).unwrap()),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        let hash = calm_server::mcp_server::auth::hash_token(token.as_str());
        session_mcp_token_set_tx(&mut tx, &session_id, &hash)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        Planner {
            track_id,
            card_id: card.id,
            session_id,
            thread_id,
            token: token.into_inner(),
            harness: None,
        }
    }

    /// A context for driving the mail module below the transport.
    pub fn app_context(&self) -> Arc<AppContext> {
        AppContext::new(
            self.repo_dyn.clone(),
            self.events.clone(),
            WriteContext::new(self.role_cache.clone(), self.area_cache.clone()),
            None,
            Arc::new(tokio::sync::OnceCell::new()),
            Arc::new(tokio::sync::OnceCell::new()),
            self.dir.path().join("gate-logs"),
        )
    }

    /// The text of every turn this Planner's thread has started, oldest first.
    pub fn turns(&self, planner: &Planner) -> Vec<String> {
        self.daemon
            .started_turns_for_test()
            .into_iter()
            .filter(|(thread, _)| *thread == planner.thread_id)
            .map(|(_, items)| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        InputItem::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect()
    }

    /// Wait for this Planner's next turn: the `n`-th started turn on its thread, running.
    pub async fn wait_turn(&self, planner: &Planner, n: usize) -> String {
        let deadline = Instant::now() + BUDGET;
        loop {
            let turns = self.turns(planner);
            let running = matches!(
                planner.harness().state_for_test().await,
                HarnessState::TurnRunning { .. }
            );
            if turns.len() >= n && running {
                return turns[n - 1].clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for turn {n}; turns={turns:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Start a turn whose input is the user's `text`; returns once it runs.
    pub async fn user_turn(&self, planner: &Planner, text: &str) {
        let before = self.turns(planner).len();
        planner
            .harness()
            .observe_for_test(Observation::UserMessage { text: text.into() }, None)
            .await;
        self.wait_turn(planner, before + 1).await;
    }

    /// Start a turn whose input is only a task completion (no user, no mail).
    pub async fn task_turn(&self, planner: &Planner) {
        let before = self.turns(planner).len();
        let completion = Observation::TaskCompleted {
            idempotency_key: new_id(),
            result: json!({"ok": true}),
        };
        planner.harness().observe_for_test(completion, None).await;
        self.wait_turn(planner, before + 1).await;
    }

    /// End the running turn and wait until the harness can issue the next one.
    pub async fn complete(&self, planner: &Planner) {
        let turn = self
            .daemon
            .active_turn_for_test(&planner.thread_id)
            .expect("a running turn");
        self.daemon
            .emit_notification_for_test(Notification::TurnCompleted {
                thread_id: planner.thread_id.clone(),
                turn: json!({ "id": turn, "status": "completed" }),
            });
        let deadline = Instant::now() + BUDGET;
        while !matches!(
            planner.harness().state_for_test().await,
            HarnessState::TurnCompleted { .. } | HarnessState::Idle
        ) {
            assert!(Instant::now() < deadline, "the turn never completed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.daemon.clear_active_turn_for_test(&planner.thread_id);
    }

    /// `tools/call` over the real transport with this Planner's token: the raw response frame.
    pub async fn call(&self, planner: &Planner, tool: &str, args: Value) -> Value {
        call_tool_card_bound(&self.socket, &planner.token, tool, args).await
    }

    /// `neige_mail_send`: `{mail_id, hop}` or the JSON-RPC error object.
    pub async fn send(&self, planner: &Planner, args: Value) -> Result<Value, Value> {
        let response = self.call(planner, "neige_mail_send", args).await;
        match response.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(structured(&response)),
        }
    }

    pub async fn send_ok(
        &self,
        planner: &Planner,
        to: &Planner,
        summary: &str,
    ) -> (String, String) {
        let sent = self
            .send(
                planner,
                json!({"track_id": to.track_id.as_str(), "summary": summary, "text": "body"}),
            )
            .await
            .unwrap_or_else(|error| panic!("send refused: {error}"));
        mail_and_hop(&sent)
    }

    pub async fn reply_ok(&self, planner: &Planner, mail_id: &str) -> (String, String) {
        let sent = self
            .send(
                planner,
                json!({"mail_id": mail_id, "summary": "reply", "text": "reply body"}),
            )
            .await
            .unwrap_or_else(|error| panic!("reply refused: {error}"));
        mail_and_hop(&sent)
    }

    /// `neige <argv>` through the forwarder protocol: `(stdout, stderr, exit)`.
    pub async fn neige(&self, planner: &Planner, argv: &[&str]) -> (String, String, i64) {
        cli_output(&neige_cli_via_socket(&self.socket, &planner.token, argv).await)
    }

    /// `neige mail cat <id> --json`.
    pub async fn cat_json(&self, planner: &Planner, mail_id: &str) -> Value {
        let (stdout, stderr, exit) = self
            .neige(planner, &["mail", "cat", mail_id, "--json"])
            .await;
        assert_eq!(exit, 0, "cat failed: {stderr}");
        serde_json::from_str(&stdout).expect("cat --json")
    }

    /// A mail row the test needs as a premise (e.g. a high hop), written straight to the table.
    pub async fn seed_mail(&self, from: &TrackId, to: &TrackId, hop: i64) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO mails (id, from_track_id, to_track_id, summary, text, hop, sent_at) \
             VALUES (?1, ?2, ?3, 'seeded', 'seeded body', ?4, ?5)",
        )
        .bind(&id)
        .bind(from.as_str())
        .bind(to.as_str())
        .bind(hop)
        .bind(now_ms())
        .execute(self.repo.pool())
        .await
        .unwrap();
        id
    }

    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    pub async fn mail_rows(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM mails").await
    }

    pub async fn wake_events(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM events WHERE kind = 'track.wake_requested'")
            .await
    }

    pub async fn read_at(&self, mail_id: &str) -> Option<i64> {
        sqlx::query_scalar("SELECT read_at FROM mails WHERE id = ?1")
            .bind(mail_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }
}

pub fn idle_snapshot(thread_id: &str) -> HarnessSnapshot {
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.to_string());
    snapshot
}

pub fn structured(response: &Value) -> Value {
    let result = &response["result"];
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text: {response:#?}"));
    serde_json::from_str(text).expect("tool result JSON")
}

pub fn mail_and_hop(sent: &Value) -> (String, String) {
    (
        sent["mail_id"].as_str().expect("mail_id").to_string(),
        sent["hop"].as_str().expect("hop").to_string(),
    )
}

/// The refusal's code and message, asserting the machine kind.
pub fn refusal(error: &Value, kind: &str) -> (i64, String) {
    assert_eq!(error["data"]["refusal"], json!(kind), "{error}");
    (
        error["code"].as_i64().expect("code"),
        error["message"].as_str().expect("message").to_string(),
    )
}
