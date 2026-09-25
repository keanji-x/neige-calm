//! A Claude Planner server for the #1791 PR4 wiring tests: the production boot
//! (`AppState::boot` on a file-backed database under a private root, so the boot revoke-then-sweep
//! runs before the real MCP listener opens), the real routes, the shared Codex daemon down (its
//! binary does not exist), and `--claude-planner-config` pointing at the fake `claude` of
//! `tests/fixtures/claude_planner_fake/claude.sh`.
//!
//! Safety: every stack has its own root, so its own `data_dir`, marker instance and fake directory;
//! the host-wide sweeps of one test can only match processes that test started. `Drop` kills what
//! still carries one of this root's markers, through the `start_time`-verified signal.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::claude_planner::stop::{MARKER_KEY, MarkerInstance, sigkill_verified_for_test};
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::harness::PlannerHarness;
use calm_server::model::{CardRole, NewArea};
use calm_server::proc_identity::read_proc_start_time;
use calm_server::routes;
use calm_server::session_projection_repo::WorkerSessionProjection;
use calm_server::state::AppState;
use calm_types::worker::WorkerSessionId;
use clap::Parser as _;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_fake/claude.sh"
);

/// The files of one server's life that outlive a reboot: database, data dir, fake, config.
pub struct Root {
    pub dir: tempfile::TempDir,
}

impl Root {
    pub fn new(scenario: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Self { dir };
        let fake = root.fake_dir();
        std::fs::create_dir_all(&fake).expect("fake dir");
        std::fs::create_dir_all(root.path().join("data")).expect("data dir");
        std::fs::copy(FAKE, fake.join("claude")).expect("copy fake");
        root.set_scenario(scenario);
        std::fs::write(
            root.config_path(),
            json!({
                "claude_binary": fake.join("claude"),
                "claude_version": "2.1.280",
                "config_dir": root.path().join("claude-config"),
            })
            .to_string(),
        )
        .expect("claude planner config");
        root
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn fake_dir(&self) -> PathBuf {
        self.path().join("fake")
    }

    fn config_path(&self) -> PathBuf {
        self.path().join("claude-planner.json")
    }

    pub fn set_scenario(&self, scenario: &str) {
        std::fs::write(self.fake_dir().join("scenario"), scenario).expect("scenario");
    }

    pub fn read_fake(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.fake_dir().join(name)).ok()
    }

    pub fn remove_fake(&self, name: &str) {
        let _ = std::fs::remove_file(self.fake_dir().join(name));
    }

    /// The last spawn's `NEIGE_MCP_TOKEN`.
    pub fn spawned_token(&self) -> String {
        self.read_fake("env")
            .expect("a spawn recorded its env")
            .lines()
            .find_map(|line| line.strip_prefix("NEIGE_MCP_TOKEN="))
            .expect("the spawn carried a token")
            .to_string()
    }

    pub fn db_url(&self) -> String {
        format!(
            "sqlite://{}?mode=rwc",
            self.path().join("calm.db").display()
        )
    }

    pub fn data_dir(&self) -> PathBuf {
        self.path().join("data")
    }

    pub fn instance(&self) -> MarkerInstance {
        MarkerInstance::for_data_dir(&self.data_dir()).expect("marker instance")
    }

    /// `with_config: false` boots the same root without `--claude-planner-config`.
    pub fn config(&self, with_config: bool) -> Config {
        let mut args = vec!["calm-server".to_string()];
        if with_config {
            args.push("--claude-planner-config".into());
            args.push(self.config_path().display().to_string());
        }
        let mut cfg = Config::parse_from(args);
        cfg.db_url = self.db_url();
        cfg.data_dir = Some(self.data_dir());
        cfg.plugins_dir = Some(self.path().join("plugins"));
        cfg.plugins_data_dir = Some(self.path().join("plugins-data"));
        cfg.workspace_root = Some(self.path().join("workspaces"));
        // Never a real agent: the shared Codex daemon cannot start.
        cfg.codex_bin = self.path().join("no-codex").display().to_string();
        cfg.claude_bin = self.path().join("no-claude").display().to_string();
        cfg
    }

    /// Spawn a detached `setsid sleep 300` carrying `worker_session_id`'s marker, as a Bash tool
    /// the CLI started would; returns `(pid, start_time)` of the sleep.
    pub fn spawn_marked_orphan(&self, worker_session_id: &str) -> (i32, u64) {
        let pid_file = self.path().join(format!("orphan-{worker_session_id}"));
        let status = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "setsid sleep 300 < /dev/null > /dev/null 2>&1 & echo $! > {}",
                pid_file.display()
            ))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env(MARKER_KEY, self.instance().marker(worker_session_id))
            .status()
            .expect("spawn orphan");
        assert!(status.success());
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("orphan pid")
            .trim()
            .parse()
            .expect("pid");
        let start_time = read_proc_start_time(pid).expect("orphan start time");
        (pid, start_time)
    }

    /// Live (non-zombie) processes carrying `worker_session_id`'s marker of this root.
    pub fn marked_pids(&self, worker_session_id: &str) -> Vec<i32> {
        marked(&HashSet::from([self.instance().marker(worker_session_id)]))
            .into_iter()
            .map(|(pid, _)| pid)
            .collect()
    }

    /// Wait until no process carries the marker (a signalled process may take a moment to go).
    pub async fn wait_unmarked(&self, worker_session_id: &str, within: Duration) -> Vec<i32> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let pids = self.marked_pids(worker_session_id);
            if pids.is_empty() || tokio::time::Instant::now() >= deadline {
                return pids;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn instructions_files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.data_dir().join("claude-planner").join("tmp"))
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let instance = self.instance().marker("");
        for (pid, start_time) in marked_by(|value| value.starts_with(&instance)) {
            sigkill_verified_for_test(pid, start_time);
        }
    }
}

/// One booted server over a [`Root`].
pub struct Stack {
    pub state: AppState,
    pub app: axum::Router,
}

impl Stack {
    /// The production boot, then the production boot recovery with the Codex daemon down.
    pub async fn boot(root: &Root) -> Self {
        Self::boot_with(root, true).await
    }

    pub async fn boot_with(root: &Root, with_config: bool) -> Self {
        let state = AppState::boot(&root.config(with_config))
            .await
            .expect("AppState::boot");
        calm_server::recover_harnesses_after_daemon_boot(
            &state,
            Err(calm_server::error::CalmError::Internal(
                "fixture: the shared codex daemon is down".into(),
            )),
        )
        .await
        .expect("boot recovery");
        let app = routes::router()
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .layer(axum::middleware::from_fn(insert_owner_principal))
            .with_state(state.clone());
        Self { state, app }
    }

    pub fn repo(&self) -> &dyn Repo {
        self.state.raw_repo()
    }

    /// Shut every live harness down (a graceful stop) so another boot of the same root is the
    /// only one issuing turns.
    pub async fn shutdown(self) {
        for handle in self.state.harness.drain_all_for_dev() {
            let _ = handle.shutdown().await;
        }
        if let Some(server) = self.state.mcp_server.as_ref() {
            server.stop_listener_for_test().await;
        }
    }

    pub async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-calm-actor", "user");
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// `POST /api/tracks` with a Claude Planner on a managed workspace; returns the track and its
    /// Planner card.
    pub async fn create_claude_track(&self) -> (String, String) {
        let area = self
            .repo()
            .area_create(NewArea {
                name: "claude planner".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .expect("area");
        let (status, body) = self
            .send(
                "POST",
                "/api/tracks",
                Some(json!({
                    "planner_provider": "claude",
                    "area_id": area.id,
                    "title": "claude planner",
                    "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let track_id = body["id"].as_str().expect("track id").to_string();
        let card = self
            .repo()
            .cards_by_track(&track_id)
            .await
            .expect("cards")
            .into_iter()
            .find(|card| self.state.write().verify_role(&card.id) == Some(CardRole::Planner))
            .expect("planner card");
        (track_id, card.id.to_string())
    }

    pub async fn runtime(&self, card_id: &str) -> WorkerSessionProjection {
        self.repo()
            .session_projection_active_for_card(&card_id.to_string())
            .await
            .expect("runtime read")
            .expect("an active runtime")
    }

    pub fn harness(&self, worker_session_id: &str) -> PlannerHarness {
        self.state
            .harness
            .get(&worker_session_id.to_string())
            .expect("a registered harness")
    }

    pub async fn post_input(&self, card_id: &str, text: &str) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/api/cards/{card_id}/planner/input"),
            Some(json!({"text": text})),
        )
        .await
    }

    /// Every stored `turn/completed` of the card, oldest first.
    pub async fn outcomes(&self, card_id: &str) -> Vec<Value> {
        super::claude_planner_session_fixture::card_rows(self.repo(), card_id, "turn/completed")
            .await
    }

    /// Wait for the card's `n`th stored outcome.
    pub async fn wait_outcomes(&self, card_id: &str, n: usize) -> Vec<Value> {
        for _ in 0..800 {
            let outcomes = self.outcomes(card_id).await;
            if outcomes.len() >= n {
                return outcomes;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "card {card_id} never reached {n} outcomes: {:?}",
            self.outcomes(card_id).await
        );
    }

    /// Run one turn under `scenario` and wait until the harness has seen it complete; returns the
    /// stored outcome and the spawn's MCP token.
    pub async fn run_turn(
        &self,
        root: &Root,
        card_id: &str,
        scenario: &str,
        text: &str,
    ) -> (Value, String) {
        root.set_scenario(scenario);
        let before = self.outcomes(card_id).await.len();
        let (status, body) = self.post_input(card_id, text).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let outcome = self.wait_outcomes(card_id, before + 1).await[before].clone();
        let runtime = self.runtime(card_id).await;
        self.wait_phase(&runtime.id, "turn_completed").await;
        (outcome, root.spawned_token())
    }

    pub async fn wait_phase(&self, worker_session_id: &str, phase: &str) {
        for _ in 0..400 {
            let snapshot = self.harness(worker_session_id).snapshot().await;
            if serde_json::to_value(snapshot.phase).unwrap() == phase {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("harness never reached {phase}");
    }

    /// `POST /api/cards/{id}/planner/reset`.
    pub async fn reset(&self, card_id: &str) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/api/cards/{card_id}/planner/reset"),
            Some(json!({})),
        )
        .await
    }

    /// Whether `token` completes the production MCP handshake right now.
    pub async fn token_authenticates(&self, token: &str) -> bool {
        calm_server::mcp_server::handshake::handle_initialize(
            self.state.repo.as_ref(),
            None,
            &json!({"_meta": {"dev.neige/auth": {"token": token}}}),
            "2024-11-05",
        )
        .await
        .is_ok()
    }

    pub async fn row_hash(&self, worker_session_id: &str) -> Option<String> {
        self.repo()
            .session_get(&WorkerSessionId(worker_session_id.to_string()))
            .await
            .expect("session_get")
            .expect("row")
            .mcp_token_hash
    }
}

async fn insert_owner_principal(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    request.extensions_mut().insert(Principal {
        user_id: "owner".into(),
        display_name: "owner".into(),
        role: "owner".into(),
        session_id: "claude-planner-stack".into(),
    });
    next.run(request).await
}

fn marked(markers: &HashSet<String>) -> Vec<(i32, u64)> {
    marked_by(|value| markers.contains(value))
}

/// `(pid, start_time)` of every live process whose marker value `accept`s; the `start_time` is
/// read before the environ.
fn marked_by(accept: impl Fn(&str) -> bool) -> Vec<(i32, u64)> {
    let prefix = format!("{MARKER_KEY}=");
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").expect("proc").flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let Some(start_time) = read_proc_start_time(pid) else {
            continue;
        };
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        let is_marked = environ.split(|&b| b == 0).any(|entry| {
            entry
                .strip_prefix(prefix.as_bytes())
                .and_then(|v| std::str::from_utf8(v).ok())
                .is_some_and(&accept)
        });
        if is_marked && super::claude_planner_session_fixture::alive(pid) {
            found.push((pid, start_time));
        }
    }
    found
}
