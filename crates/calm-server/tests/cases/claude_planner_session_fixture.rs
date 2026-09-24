//! The fake-`claude` rig for the #1791 session tests: a private directory holding the fake CLI
//! (`tests/fixtures/claude_planner_fake/claude.sh`), a data dir, a workspace, and an in-memory
//! repo with a Planner card, around a real `ClaudePlannerSession`.
//!
//! Safety: every rig mints a fresh `data_dir` (so a fresh marker instance) and a fresh worker
//! session id, so the host-wide sweep of one test can only match processes that test started. The
//! rig's `Drop` kills whatever still carries its own marker.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::claude_planner::config::{ClaudePlannerConfig, ClaudePlannerHost};
use calm_server::claude_planner::session::{ClaudePlannerSession, ClaudePlannerSessionParams};
use calm_server::claude_planner::spawn::SessionStart;
use calm_server::claude_planner::stop::MARKER_KEY;
use calm_server::claude_planner::translate::CalmToolNames;
use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::{Value, json};
use tokio::sync::broadcast;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_fake/claude.sh"
);
pub const P_D_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_stream/pB_emptycfg.ndjson"
);

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub session: Arc<ClaudePlannerSession>,
    pub host: Arc<ClaudePlannerHost>,
    pub repo: Arc<SqlxRepo>,
    pub daemon: Arc<SharedCodexAppServer>,
    pub card_id: String,
    pub worker_session_id: String,
    pub thread: String,
}

impl Rig {
    pub async fn new(scenario: &str) -> Self {
        Self::with_instructions(scenario, "Planner instructions for the fake.").await
    }

    pub async fn with_instructions(scenario: &str, instructions: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        let data = dir.path().join("data");
        let ws = dir.path().join("ws");
        for path in [&bin, &data, &ws] {
            std::fs::create_dir_all(path).expect("mkdir");
        }
        std::fs::copy(FAKE, bin.join("claude")).expect("copy fake");
        std::fs::write(bin.join("scenario"), scenario).expect("scenario");

        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("repo"));
        let (card_id, track_id) = planner_card(&repo).await;
        let config = ClaudePlannerConfig {
            claude_binary: bin.join("claude"),
            claude_version: "2.1.280".into(),
            config_dir: dir.path().join("claude-config"),
        };
        let host = Arc::new(
            ClaudePlannerHost::new(
                config,
                &data,
                PathBuf::from("/nonexistent/neige-mcp-stdio-shim"),
                dir.path().join("mcp.sock"),
            )
            .expect("host"),
        );
        let daemon = SharedCodexAppServer::new_stub(repo.clone());
        let worker_session_id = new_id();
        let session = Arc::new(ClaudePlannerSession::new(ClaudePlannerSessionParams {
            host: Arc::clone(&host),
            worker_session_id: worker_session_id.clone(),
            card_id: card_id.clone(),
            track_id,
            cwd: ws,
            instructions: instructions.to_string(),
            calm_tools: CalmToolNames::new(["calm.report.write".to_string()]),
            proxy: Vec::new(),
            start: SessionStart::New,
            prior_total_tokens: 0,
            repo: repo.clone(),
            seals: Arc::clone(&daemon),
        }));
        session.install_mcp_token("tok-rig".into());
        Self {
            dir,
            session,
            host,
            repo,
            daemon,
            card_id,
            worker_session_id,
            thread: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn bin(&self, name: &str) -> PathBuf {
        self.dir.path().join("bin").join(name)
    }

    pub fn read_bin(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.bin(name)).ok()
    }

    pub fn marker(&self) -> String {
        self.host.instance.marker(&self.worker_session_id)
    }

    /// Live (non-zombie) processes carrying this rig's exact marker.
    pub fn marked_pids(&self) -> Vec<i32> {
        marked_pids(&HashSet::from([self.marker()]))
    }

    pub fn instructions_files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.host.instructions_dir)
            .expect("instructions dir")
            .flatten()
            .map(|entry| entry.path())
            .collect()
    }

    pub async fn outcomes(&self) -> Vec<Value> {
        self.repo
            .harness_item_list_by_card(&self.card_id, 0, 1000, false)
            .await
            .expect("list")
            .into_iter()
            .filter(|row| row.method == "turn/completed")
            .map(|row| serde_json::from_str(&row.params).expect("params"))
            .collect()
    }

    pub fn text(&self, text: &str) -> Vec<InputItem> {
        vec![InputItem::Text { text: text.into() }]
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for pid in self.marked_pids() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

pub fn client_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

async fn planner_card(repo: &Arc<SqlxRepo>) -> (String, String) {
    let area = repo
        .area_create(NewArea {
            name: "claude-planner".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .expect("area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "claude planner".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("track");
    let mut tx = repo.pool().begin().await.expect("tx");
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &CardRoleCache::new(),
    )
    .await
    .expect("card");
    tx.commit().await.expect("commit");
    (card.id.to_string(), track.id.to_string())
}

fn marked_pids(markers: &HashSet<String>) -> Vec<i32> {
    let prefix = format!("{MARKER_KEY}=");
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").expect("proc").flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        let marked = environ.split(|&b| b == 0).any(|entry| {
            entry
                .strip_prefix(prefix.as_bytes())
                .and_then(|v| std::str::from_utf8(v).ok())
                .is_some_and(|v| markers.contains(v))
        });
        if marked && alive(pid) {
            pids.push(pid);
        }
    }
    pids
}

/// Present in `/proc` and not a zombie.
pub fn alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_string))
        })
        .is_some_and(|state| state != "Z")
}

pub fn cmdline(pid: i32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default()
}

/// Every notification up to and including the next `TurnCompleted`.
pub async fn until_completed(rx: &mut broadcast::Receiver<Notification>) -> Vec<Notification> {
    let mut seen = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(40), rx.recv())
            .await
            .expect("TurnCompleted within 40 s")
            .expect("notification");
        let done = matches!(next, Notification::TurnCompleted { .. });
        seen.push(next);
        if done {
            return seen;
        }
    }
}

pub fn completed_turn(seen: &[Notification]) -> Value {
    match seen.last() {
        Some(Notification::TurnCompleted { turn, .. }) => turn.clone(),
        other => panic!("expected TurnCompleted last, got {other:?}"),
    }
}

/// Wait until `path` exists (the fake writes its records as it goes).
pub async fn wait_for_file(path: &Path) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{} never appeared", path.display());
}
