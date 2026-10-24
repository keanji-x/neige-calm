//! A Claude Planner server for the #1791 PR4 wiring tests: the production boot
//! (`AppState::boot` on a file-backed database under a private root, so the boot revoke-then-sweep
//! runs before the real MCP listener opens), the real routes, the shared Codex daemon down (its
//! binary does not exist), and `--claude-planner-config` pointing at the fake `claude` of
//! `tests/fixtures/claude_planner_fake/claude.sh`.
//!
//! Safety: every stack has its own root, so its own `data_dir`, marker instance and fake directory;
//! the host-wide sweeps of one test can only match processes that test started. `Drop` kills what
//! still carries one of this root's markers, plus retained orphan identities, through the
//! `start_time`-verified signal.

use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::claude_planner::config::MARKER_KEY;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::harness::PlannerHarness;
use calm_server::model::{CardRole, NewArea};
use calm_server::planner_process::{MarkerInstance, sigkill_verified_for_test};
use calm_server::proc_identity::read_proc_start_time;
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

// Separate private streams keep the final ready report independent of its builtin read.
const ORPHAN_CHILD: &str = r#"printf 'ready %s\n' "$BASHPID"; IFS= read -r hold"#;

struct Orphan {
    pid: i32,
    start_time: u64,
    hold: UnixStream,
}

impl Drop for Orphan {
    fn drop(&mut self) {
        sigkill_verified_for_test(self.pid, self.start_time);
    }
}

/// The files of one server's life that outlive a reboot: database, data dir, fake, config.
pub struct Root {
    // Owned by the test, so every boot over this root uses the same supervisor.
    execution: crate::support::execution::Fixture,
    pub dir: tempfile::TempDir,
    orphans: Mutex<Vec<Orphan>>,
}

impl Root {
    pub fn new(scenario: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Self {
            dir,
            orphans: Mutex::new(Vec::new()),
            execution: crate::support::execution::Fixture::start(),
        };
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

    pub fn assert_execution_journal_excludes(&self, needle: &str) {
        self.execution.assert_journal_excludes(needle);
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
        MarkerInstance::for_data_dir(
            &self.data_dir(),
            calm_server::claude_planner::config::MARKER_KEY,
        )
        .expect("marker instance")
    }

    /// `with_config: false` boots the same root without `--claude-planner-config`.
    pub fn config(&self, with_config: bool) -> Config {
        let mut args = vec!["calm-server".to_string()];
        if with_config {
            args.push("--claude-planner-config".into());
            args.push(self.config_path().display().to_string());
        }
        let mut cfg = Config::parse_from(args);
        self.execution.configure(&mut cfg);
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

    /// The final setsid child acknowledges its own PID, then blocks in a Bash builtin.
    /// It never execs or forks after readiness. The launcher is already reaped on return.
    pub fn spawn_marked_orphan(&self, worker_session_id: &str) -> (i32, u64) {
        self.try_spawn_marked_orphan(worker_session_id, ORPHAN_CHILD, |_, _, _, _| {})
            .expect("prepare marked orphan")
    }

    fn try_spawn_marked_orphan(
        &self,
        worker_session_id: &str,
        child_script: &str,
        // The contract regressions control the final child before its readiness report.
        before_ready: impl FnOnce(i32, u64, &mut UnixStream, &mut BufReader<UnixStream>),
    ) -> anyhow::Result<(i32, u64)> {
        let files = tempfile::tempdir_in(self.path())?;
        let pid_file = files.path().join("pid");
        let (ready, report) = UnixStream::pair()?;
        ready.set_read_timeout(Some(Duration::from_secs(5)))?;
        let (hold, input) = UnixStream::pair()?;
        let mut launcher = std::process::Command::new("/bin/bash")
            .args([
                "-c",
                r#"setsid /bin/bash -c "$1" orphan <&0 & printf '%s\n' "$!" > "$2""#,
                "launcher",
            ])
            .arg(child_script)
            .arg(&pid_file)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env(MARKER_KEY, self.instance().marker(worker_session_id))
            .stdin(std::process::Stdio::from(OwnedFd::from(input)))
            .stdout(std::process::Stdio::from(OwnedFd::from(report)))
            .stderr(std::process::Stdio::null())
            .spawn()?;
        // Reap the launcher before accepting the detached final child.
        let status = launcher.wait()?;
        anyhow::ensure!(status.success(), "orphan launcher failed: {status}");
        let pid: i32 = std::fs::read_to_string(pid_file)?.trim().parse()?;
        anyhow::ensure!(
            pid > 1 && pid != std::process::id() as i32,
            "invalid launch PID"
        );
        let start_time = read_proc_start_time(pid)
            .ok_or_else(|| anyhow::anyhow!("orphan exited before readiness"))?;
        // Capture cleanup ownership before reading the final ack or inspecting its marker.
        // In particular, failed preparation must not depend on a later marker scan.
        let mut orphan = Orphan {
            pid,
            start_time,
            hold,
        };
        let mut ready = BufReader::new(ready);
        before_ready(pid, start_time, &mut orphan.hold, &mut ready);
        let mut frame = String::new();
        let count = ready.by_ref().take(128).read_line(&mut frame)?;
        anyhow::ensure!(count > 0, "orphan readiness EOF");
        self.verify_orphan_ready(worker_session_id, pid, start_time, &frame)?;
        anyhow::ensure!(
            // SAFETY: getsid only queries the positive PID captured above.
            unsafe { libc::getsid(pid) } == pid,
            "orphan is not a session leader"
        );
        self.orphans.lock().unwrap().push(orphan);
        Ok((pid, start_time))
    }

    fn verify_orphan_ready(
        &self,
        worker_session_id: &str,
        pid: i32,
        start_time: u64,
        frame: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            frame == format!("ready {pid}\n"),
            "bad orphan readiness identity: {frame:?}"
        );
        anyhow::ensure!(
            read_proc_start_time(pid) == Some(start_time),
            "orphan identity changed"
        );
        anyhow::ensure!(
            super::claude_planner_session_fixture::alive(pid),
            "orphan is not live"
        );
        // Reuse the fixture's exact marker decoder, not a second environment parser.
        anyhow::ensure!(
            marked(&HashSet::from([self.instance().marker(worker_session_id)]))
                .contains(&(pid, start_time)),
            "orphan marker is not ready"
        );
        Ok(())
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
        self.orphans.get_mut().unwrap().clear();
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
        Self::boot_config(&root.config(with_config)).await
    }

    pub async fn boot_config(config: &Config) -> Self {
        let mut config = config.clone();
        config.auth_password = Some("fixture-password".into());
        let state = AppState::boot(&config)
            .await
            .expect("AppState::boot")
            .into_parts()
            .0;
        let app = crate::support::authenticated_router::application(state.clone()).await;
        Self { state, app }
    }

    pub fn repo(&self) -> &dyn Repo {
        self.state.raw_repo()
    }

    pub async fn block_provider_stop(&self) {
        sqlx::query(
            "CREATE TRIGGER c4_provider_fault BEFORE UPDATE OF desired ON workspace_executions
             WHEN NEW.desired='stop' BEGIN SELECT RAISE(FAIL, 'provider Stop blocked'); END",
        )
        .execute(self.repo().sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    }

    pub async fn block_provider_admission(&self) {
        sqlx::query(
            "CREATE TRIGGER c4_provider_fault BEFORE INSERT ON workspace_executions
             BEGIN SELECT RAISE(FAIL, 'provider admission blocked'); END",
        )
        .execute(self.repo().sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    }

    pub async fn clear_provider_fault(&self) {
        sqlx::query("DROP TRIGGER c4_provider_fault")
            .execute(self.repo().sqlite_db().unwrap().writer_pool())
            .await
            .unwrap();
    }

    pub async fn wait_provider_clear(&self, session: &str) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while calm_truth::db::sqlite::provider_execution::load(
                self.repo().sqlite_db().unwrap().read_pool(),
                session,
            )
            .await
            .unwrap()
            .is_some()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("provider completion clears its claim");
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
        self.send_with(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("x-calm-actor", "user"),
            body,
        )
        .await
    }

    /// `POST /planner/input` as one new send: a fresh `Idempotency-Key`.
    pub async fn send_input(&self, card_id: &str, body: Value) -> (StatusCode, Value) {
        self.send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/planner/input"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", calm_server::model::new_id()),
            Some(body),
        )
        .await
    }

    pub async fn send_with(
        &self,
        builder: axum::http::request::Builder,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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
        self.create_claude_track_with(json!({})).await
    }

    /// A fresh area for a create body.
    pub async fn area(&self) -> String {
        self.repo()
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "claude planner".into(),
                    color: "#111111".into(),
                    sort: None,
                },
            )
            .await
            .expect("area")
            .id
            .to_string()
    }

    /// [`Self::create_claude_track`] with `extra` merged into the create body.
    pub async fn create_claude_track_with(&self, extra: Value) -> (String, String) {
        let mut request = json!({
            "planner_provider": "claude",
            "area_id": self.area().await,
            "title": "claude planner",
            "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
        });
        // These legacy lifecycle consumers assert eager historical startup,
        // queue replay and native outcomes. With cwd omitted, attach_folder
        // explicitly selects historical creation while preserving Managed isolation.
        if extra.get("cwd").is_none() {
            request["template_id"] = json!("small-change");
            request["attach_folder"] = json!(true);
        }
        for (key, value) in extra.as_object().expect("extra is an object") {
            request[key] = value.clone();
        }
        let (status, body) = self.send("POST", "/api/tracks", Some(request)).await;
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
        self.send_input(card_id, json!({"text": text})).await
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

    /// Wait for the current turn's phase to be committed, not just visible in memory.
    pub async fn wait_phase(&self, worker_session_id: &str, phase: &str) {
        for _ in 0..400 {
            let snapshot = self.harness(worker_session_id).snapshot().await.unwrap();
            if serde_json::to_value(snapshot.phase).unwrap() == phase {
                let persisted = self
                    .repo()
                    .session_projection_by_id(worker_session_id)
                    .await
                    .unwrap()
                    .expect("the worker session row")
                    .handle_state_json
                    .expect("a persisted snapshot");
                // A previous turn can have the same phase while this turn's write is pending.
                if persisted["phase"] == phase
                    && persisted["last_turn_id"] == json!(snapshot.last_turn_id)
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("harness never persisted {phase} for the current turn");
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

#[test]
fn orphan_readiness_waits_for_the_final_child() {
    let root = Root::new("exit");
    let script = format!("printf 'entered\\n'; IFS= read -r permit; {ORPHAN_CHILD}");
    let (pid, stamp) = root
        .try_spawn_marked_orphan("barrier", &script, |_, _, permit, ready| {
            let mut entered = String::new();
            ready.read_line(&mut entered).unwrap();
            assert_eq!(entered, "entered\n");
            eprintln!("ChildEntered; WaitReadyEntered; final ack withheld until permit");
            permit.write_all(b"go\n").unwrap();
        })
        .unwrap();
    eprintln!("HelperReturned after final ready: pid={pid} start_time={stamp}");
    assert_eq!(root.marked_pids("barrier"), vec![pid]);
    assert!(
        root.verify_orphan_ready("barrier", pid, stamp + 1, &format!("ready {pid}\n"))
            .is_err()
    );
}

#[test]
fn orphan_bad_preparation_fails_and_cleans_known_identity() {
    let root = Root::new("exit");
    let unmarked = format!(
        "exec /usr/bin/env -u {MARKER_KEY} /bin/bash -c '{}'",
        ORPHAN_CHILD.replace('\'', "'\\''")
    );
    for (id, script, expected) in [
        ("eof", "exec 1>&-; IFS= read -r hold", "readiness EOF"),
        (
            "bad",
            "printf 'ready 1\\n'; IFS= read -r hold",
            "bad orphan readiness identity",
        ),
        ("exit", "exit 0", "orphan"),
        ("unmarked", unmarked.as_str(), "orphan marker is not ready"),
    ] {
        let mut held = None;
        let error = root
            .try_spawn_marked_orphan(id, script, |pid, stamp, hold, _| {
                eprintln!("{id}: known launch pid={pid} start_time={stamp}");
                held = Some(hold.try_clone().unwrap());
            })
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{id}: {error:#}");
        if let Some(mut held) = held {
            held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut byte = [0];
            assert_eq!(
                held.read(&mut byte).unwrap(),
                0,
                "failed preparation left its child alive: {id}"
            );
        }
        eprintln!("{id}: rejected and launch channel closed: {error}");
    }
    assert!(root.orphans.lock().unwrap().is_empty());
}

#[test]
fn orphan_drop_preserves_another_roots_same_session() {
    let root = Root::new("exit");
    let other = Root::new("exit");
    root.spawn_marked_orphan("same");
    let (pid, stamp) = other.spawn_marked_orphan("same");
    drop(root);
    assert_eq!(other.marked_pids("same"), vec![pid]);
    assert_eq!(read_proc_start_time(pid), Some(stamp));
}
