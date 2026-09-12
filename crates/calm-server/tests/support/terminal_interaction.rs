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

const FAKE_BRIDGE: &str = r#"#!/bin/sh
# test stand-in for neige-codex-bridge: same env contract (NEIGE_HOOK_URL),
# same per-invocation occurrence stamp (#1620); POSTs stdin as the hook body.
body=$(cat)
occurrence="$$-$(date +%s%3N)-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
case "$body" in
  *\}) body="${body%\}},\"neige_hook_occurrence\":\"$occurrence\"}" ;;
esac
printf '%s' "$body" | curl -sS --noproxy '*' -o /dev/null -X POST -H 'content-type: application/json' -H 'X-Calm-Actor: ai:claude' --data-binary @- "$NEIGE_HOOK_URL"
printf '{"continue":true}'
"#;

pub struct Harness {
    pub sql: Arc<SqlxRepo>,
    pub state: AppState,
    server: Arc<McpServer>,
    supervisor: InProcessProcSupervisor,
    pub root: tempfile::TempDir,
    socket: PathBuf,
    pub token: String,
    pub track: String,
    /// #1620 — the production REST router over the same state, for hook
    /// POSTs (`tower::ServiceExt::oneshot`) and card deletes.
    pub app: axum::Router,
    /// Loopback HTTP server serving `app`, the `NEIGE_CALM_BASE_URL` a
    /// terminal's bridge command POSTs to.
    pub base_url: String,
    /// Test stand-in for `neige-codex-bridge` honoring the same env contract.
    pub bridge: PathBuf,
    http: tokio::task::JoinHandle<()>,
}
impl Harness {
    pub fn interaction(&self) -> Arc<TerminalInteraction> {
        self.server.terminal_interaction.get().unwrap().clone()
    }
    pub fn supervisor_socket(&self) -> PathBuf {
        self.supervisor.sock().to_owned()
    }
    pub async fn start() -> Self {
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
        // #1620 — a real loopback ingest endpoint and a bridge stand-in so a
        // terminal's generated hook command reaches `/internal/claude/hook`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let bridge = root.path().join("fake-bridge.sh");
        // Same env contract (NEIGE_HOOK_URL) and the same per-invocation
        // `neige_hook_occurrence` stamp as the real bridge (#1620, see
        // calm-codex-bridge/src/main.rs); POSTs stdin as the hook body.
        std::fs::write(&bridge, FAKE_BRIDGE).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bridge, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut codex = CodexClient::new_stub();
        codex.bridge_bin = bridge.clone();
        codex.ingest_url = base_url.clone();
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
            Arc::new(codex),
            Some(roles),
            Some(areas),
        );
        let app = calm_server::routes::router()
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .with_state(state.clone());
        let served = app.clone();
        let http = tokio::spawn(async move {
            axum::serve(listener, served).await.unwrap();
        });
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
            sql,
            state,
            server,
            supervisor,
            root,
            socket,
            token: token.expect("planner MCP token"),
            track: track.id.to_string(),
            app,
            base_url,
            bridge,
            http,
        }
    }
    /// POST a hook body for `card_id` through the production ingest route.
    pub async fn post_claude_hook(&self, card_id: &str, body: &Value) -> axum::http::StatusCode {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/internal/claude/hook?card_id={card_id}"))
            .header("content-type", "application/json")
            .header("X-Calm-Actor", "ai:claude")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap().status()
    }
    pub async fn call(&self, name: &str, args: Value) -> Value {
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
    pub async fn ok(&self, name: &str, args: Value) -> Value {
        let response = self.call(name, args).await;
        assert!(response.get("error").is_none(), "{response}");
        response["result"]["structuredContent"].clone()
    }
    pub async fn observe_text(&self, terminal: &str, needle: &str) -> Value {
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
    pub async fn input(&self, terminal: &str, view: &Value, key: &str, action: Value) -> Value {
        self.ok("calm.terminal.input",json!({"terminal_id":terminal,"observation_id":view["observation_id"],"request_id":key,"action":action})).await
    }
    pub async fn stop(self, terminal: &str) {
        self.state.terminal_renderer.drop_entry(terminal).await;
        self.http.abort();
        drop(self.server);
        drop(self.state);
        drop(self.supervisor);
        drop(self.root);
    }
}

/// Inspect the complete MCP envelope, including the textual projection of metadata.
pub fn assert_text_observation(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    let content = response["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "text observation must not include images");
    assert_eq!(content[0]["type"], "text");
    let metadata = &response["result"]["structuredContent"];
    // #1618: the text block is a one-line summary, never a second copy of the
    // state and never the screen text.
    let summary = content[0]["text"].as_str().unwrap();
    assert!(!summary.contains('\n'), "{summary}");
    assert!(
        summary.ends_with("; full state in structuredContent"),
        "{summary}"
    );
    assert!(
        summary.contains(&format!(
            "observation {} revision {} ",
            metadata["observation_id"].as_str().unwrap(),
            metadata["observation_revision"].as_str().unwrap()
        )),
        "{summary}"
    );
    assert!(serde_json::from_str::<Value>(summary).is_err());
    assert!(
        metadata.get("image_source").is_none(),
        "text observation must not claim an image source"
    );
    uuid::Uuid::parse_str(metadata["observation_id"].as_str().unwrap()).unwrap();
    uuid::Uuid::parse_str(metadata["connection_id"].as_str().unwrap()).unwrap();
    assert!(metadata["text"].is_array());
    assert!(metadata["cursor"].is_object());
    assert!(metadata["cols"].as_u64().unwrap() > 0);
    assert!(metadata["rows"].as_u64().unwrap() > 0);
    metadata
}
