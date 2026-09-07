//! Production MCP + operation + renderer + real PTY; no model process.
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_with_codex_create_tx};
use calm_server::event::EventBus;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, NewArea, NewTrack, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_interaction::TerminalInteraction;
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

struct Harness {
    state: AppState,
    server: Arc<McpServer>,
    supervisor: InProcessProcSupervisor,
    root: tempfile::TempDir,
    socket: PathBuf,
    token: String,
    track: String,
}
impl Harness {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let sql = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let repo: Arc<dyn Repo> = sql.clone();
        let area = repo
            .area_create(NewArea {
                name: "terminal-tool".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "terminal".into(),
                sort: None,
                cwd: root.path().to_str().unwrap().into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let roles = CardRoleCache::new();
        let areas = TrackAreaCache::new();
        repo.seed_track_area_cache(&areas).await.unwrap();
        let mut tx = sql.pool().begin().await.unwrap();
        let (_, _, token) = card_with_codex_create_tx(
            &mut tx,
            new_id(),
            &new_id(),
            None,
            track.id.clone(),
            None,
            None,
            root.path().to_str().unwrap().into(),
            json!({}),
            None,
            None,
            None,
            CardRole::Planner,
            false,
            &roles,
            calm_server::routes::theme::RequestTheme::default_dark(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let supervisor = InProcessProcSupervisor::start().await.unwrap();
        let events = EventBus::new();
        let write = WriteContext::new(roles.clone(), areas.clone());
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            root.path().join("plugins"),
            vec![],
            EventBus::new(),
            write.clone(),
        ));
        let state = AppState::from_parts(
            repo.clone(),
            events.clone(),
            Arc::new(DaemonClient {
                data_dir: root.path().join("terminals"),
                proc_supervisor_sock: Some(supervisor.sock().to_owned()),
            }),
            plugin,
            Arc::new(CodexClient::new_stub()),
            Some(roles),
            Some(areas),
        );
        let operations = Arc::new(tokio::sync::OnceCell::new());
        operations
            .set(state.operation_runtime.clone())
            .ok()
            .unwrap();
        let socket = root.path().join("mcp.sock");
        let server = McpServer::spawn(
            repo.clone(),
            events,
            write,
            socket.clone(),
            PathBuf::from("unused-shim"),
            build_default_registry(),
            None,
            Arc::new(tokio::sync::OnceCell::new()),
            operations,
            root.path().join("gates"),
            100,
        )
        .await
        .unwrap();
        server
            .terminal_interaction
            .set(Arc::new(TerminalInteraction::new(
                repo.clone(),
                state.terminal_renderer.clone(),
            )))
            .ok()
            .unwrap();
        Self {
            state,
            server,
            supervisor,
            root,
            socket,
            token: token.expect("planner MCP token"),
            track: track.id.to_string(),
        }
    }
    async fn call(&self, name: &str, args: Value) -> Value {
        let stream = UnixStream::connect(&self.socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"terminal-test","version":"1"},"_meta":{"dev.neige/auth":{"token":self.token}}}});
        write
            .write_all(format!("{init}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        let initialized: Value = serde_json::from_str(&line).unwrap();
        assert!(initialized.get("error").is_none(), "{initialized}");
        let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}});
        write
            .write_all(format!("{call}\n").as_bytes())
            .await
            .unwrap();
        line.clear();
        tokio::time::timeout(Duration::from_secs(20), read.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn ok(&self, name: &str, args: Value) -> Value {
        let response = self.call(name, args).await;
        assert!(response.get("error").is_none(), "{response}");
        response["result"]["structuredContent"].clone()
    }
    async fn observe_text(&self, terminal: &str, needle: &str) -> Value {
        let start = std::time::Instant::now();
        loop {
            let view = self
                .ok(
                    "calm.terminal.observe",
                    json!({"terminal_id":terminal,"wait_ms":30}),
                )
                .await;
            if view["text"]
                .as_array()
                .unwrap()
                .iter()
                .any(|line| line.as_str().unwrap().contains(needle))
            {
                return view;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "missing {needle}: {view}"
            );
        }
    }
    async fn input(&self, terminal: &str, view: &Value, key: &str, action: Value) -> Value {
        self.ok("calm.terminal.input",json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":key,"action":action})).await
    }
    async fn stop(self, terminal: &str) {
        self.state.terminal_renderer.drop_entry(terminal).await;
        drop(self.server);
        drop(self.state);
        drop(self.supervisor);
        drop(self.root);
    }
}

#[tokio::test]
async fn planner_opens_visible_terminal_and_receives_png_and_confirmed_input() {
    let h = Harness::start().await;
    let opened = h
        .call(
            "calm.terminal.open",
            json!({"request_id":"open-1","title":"Planner terminal"}),
        )
        .await;
    assert!(opened.get("error").is_none(), "{opened}");
    let meta = &opened["result"]["structuredContent"];
    let terminal = meta["terminal_id"].as_str().unwrap().to_owned();
    assert!(
        opened["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "image" && part["mimeType"] == "image/png")
    );
    let card = h
        .state
        .repo
        .card_get(meta["card_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.track_id.as_str(), h.track);
    assert_eq!(card.kind, "terminal");
    let repeated = h
        .ok(
            "calm.terminal.open",
            json!({"request_id":"open-1","title":"Planner terminal"}),
        )
        .await;
    assert_eq!(repeated["terminal_id"], terminal);
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let view = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    assert_eq!(
        h.input(
            &terminal,
            &view,
            "text-1",
            json!({"type":"text","text":"printf 'PLANNER_TERMINAL_OK\\n'"})
        )
        .await["outcome"],
        "written"
    );
    let typed = h.observe_text(&terminal, "printf").await;
    let first = h
        .input(
            &terminal,
            &typed,
            "enter-1",
            json!({"type":"key","key":"Enter"}),
        )
        .await;
    assert_eq!(first["outcome"], "written");
    let repeat = h
        .input(
            &terminal,
            &typed,
            "enter-1",
            json!({"type":"key","key":"Enter"}),
        )
        .await;
    assert_eq!(
        first, repeat,
        "same request must replay its receipt without writing again"
    );
    let result = h.observe_text(&terminal, "PLANNER_TERMINAL_OK").await;
    assert_eq!(result["terminal_session_id"], view["terminal_session_id"]);
    h.stop(&terminal).await;
}

#[tokio::test]
async fn planner_terminal_refuses_unowned_and_cross_track_input() {
    let h = Harness::start().await;
    let open = h
        .ok("calm.terminal.open", json!({"request_id":"no-owner"}))
        .await;
    let terminal = open["terminal_id"].as_str().unwrap().to_owned();
    let denied=h.call("calm.terminal.input",json!({"terminal_id":terminal,"observation_id":open["observation_id"],"request_id":"denied","action":{"type":"text","text":"bad"}})).await;
    assert!(denied.get("error").is_some());
    let other = Harness::start().await;
    let foreign = other
        .call("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert!(foreign.get("error").is_some());
    h.stop(&terminal).await;
}
