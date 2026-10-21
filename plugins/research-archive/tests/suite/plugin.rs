//! The plugin binary over stdio, against a fake kernel that answers its
//! `neige.kv.*` and `neige.overlay.*` callbacks from memory.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use regex::Regex;
use research_archive::Store;
use research_archive::lock::DataDirLock;
use research_archive::state::{ExitKind, LastExit};
use serde_json::{Value, json};

use crate::support::complete_day;

const TRACK: &str = "0123456789abcdef0123456789abcdef";
const OVERLAY: &str = "archive.status";

pub(crate) struct Kernel {
    child: Child,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Value>,
    next_id: u64,
    kv: BTreeMap<String, Value>,
    /// `(track, kind)` → payload.
    overlays: BTreeMap<(String, String), Value>,
    /// Overlay pushes, by track.
    sets: BTreeMap<String, usize>,
}

impl Kernel {
    pub(crate) fn spawn(config: Value) -> Kernel {
        let mut child = Command::new(env!("CARGO_BIN_EXE_research-archive-plugin"))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                let frame: Value = serde_json::from_str(&line).expect("the plugin writes JSON");
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });
        let mut kernel = Kernel {
            child,
            stdin,
            frames,
            next_id: 1,
            kv: BTreeMap::new(),
            overlays: BTreeMap::new(),
            sets: BTreeMap::new(),
        };
        let reply = kernel.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "_meta": {
                    "dev.neige/auth": { "expected_echo": "echo-me" },
                    "dev.neige/config": { "values": config },
                },
            }),
        );
        assert_eq!(
            reply.pointer("/result/_meta/dev.neige~1auth/echoed_token"),
            Some(&json!("echo-me"))
        );
        kernel
    }

    fn send(&mut self, frame: Value) {
        writeln!(self.stdin, "{frame}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and serves callbacks until its reply comes.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.serve_until(|frame| frame.get("method").is_none() && frame["id"] == json!(id))
    }

    pub(crate) fn call_tool(&mut self, name: &str, arguments: Value, track: Option<&str>) -> Value {
        let mut params = json!({ "name": name, "arguments": arguments });
        if let Some(track) = track {
            params["_meta"] = json!({ "dev.neige/track": { "id": track } });
        }
        self.request("tools/call", params)["result"].clone()
    }

    /// The next frame from the plugin, by `deadline`.
    fn next_frame(&self, deadline: Instant) -> Value {
        let left = deadline.saturating_duration_since(Instant::now());
        self.frames
            .recv_timeout(left)
            .expect("the plugin answers in time")
    }

    /// Answers every callback until `done` matches a frame, which it returns.
    fn serve_until(&mut self, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let frame = self.next_frame(deadline);
            if done(&frame) {
                return frame;
            }
            self.answer(&frame);
        }
    }

    /// Answers one callback from the in-memory KV and overlays.
    fn answer(&mut self, frame: &Value) {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            panic!("an unexpected reply: {frame}");
        };
        let params = &frame["params"];
        let result = match method {
            "neige.kv.list" => {
                let prefix = params["prefix"].as_str().unwrap();
                let entries: Vec<Value> = self
                    .kv
                    .iter()
                    .filter(|(k, _)| k.starts_with(prefix))
                    .map(|(k, v)| json!({ "key": k, "value": v }))
                    .collect();
                json!({ "entries": entries })
            }
            "neige.kv.set" => {
                self.kv.insert(
                    params["key"].as_str().unwrap().to_owned(),
                    params["value"].clone(),
                );
                json!({})
            }
            "neige.kv.delete" => {
                self.kv.remove(params["key"].as_str().unwrap());
                json!({})
            }
            "neige.overlay.set" => {
                assert_eq!(params["entity_kind"], "track");
                let payload = params["payload"].clone();
                calm_types::report_blocks::kinds::validate_inline_table_overlay(&payload)
                    .unwrap_or_else(|e| panic!("not a table: {e}: {payload}"));
                let track = params["entity_id"].as_str().unwrap().to_owned();
                *self.sets.entry(track.clone()).or_default() += 1;
                self.overlays.insert(
                    (track, params["kind"].as_str().unwrap().to_owned()),
                    payload,
                );
                json!({})
            }
            "neige.overlay.delete" => {
                assert_eq!(params["entity_kind"], "track");
                self.overlays.remove(&(
                    params["entity_id"].as_str().unwrap().to_owned(),
                    params["kind"].as_str().unwrap().to_owned(),
                ));
                json!({})
            }
            other => panic!("an unexpected callback {other}: {frame}"),
        };
        self.send(json!({ "jsonrpc": "2.0", "id": frame["id"], "result": result }));
    }

    /// Serves callbacks until the table on `track` satisfies `f`.
    fn until_overlay(&mut self, track: &str, f: impl Fn(Option<&Value>) -> bool) {
        let key = (track.to_owned(), OVERLAY.to_owned());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f(self.overlays.get(&key)) {
            let frame = self.next_frame(deadline);
            self.answer(&frame);
        }
    }

    /// Serves callbacks for `period`, answering whatever comes.
    fn serve_for(&mut self, period: Duration) {
        let until = Instant::now() + period;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if let Ok(frame) = self.frames.recv_timeout(left) {
                self.answer(&frame);
            }
        }
    }

    /// The overlay pushes `track` has had.
    fn sets(&self, track: &str) -> usize {
        self.sets.get(track).copied().unwrap_or_default()
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn config(dir: &Path) -> Value {
    json!({
        "data_dir": dir.to_str().unwrap(),
        "poll_seconds": 10,
        "stale_after_minutes": 90,
    })
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn last_exit(dir: &Path, kind: ExitKind) {
    LastExit {
        invocation: None,
        code: kind.code(),
        kind,
        at: now_ms(),
        as_of: Some("2026-01-02".into()),
        failure: None,
        closed_dangling: Vec::new(),
        missed_scan_incomplete: false,
    }
    .write(dir);
}

async fn healthy_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    complete_day(&store, 1, &["AAPL.US"], ".SPX.US", &["AAPL.US", "MSFT.US"]).await;
    store.close().await;
    last_exit(dir.path(), ExitKind::Complete);
    dir
}

fn caption(table: Option<&Value>) -> &str {
    table
        .and_then(|t| t["caption"].as_str())
        .unwrap_or_default()
}

#[tokio::test]
async fn status_answers_with_the_health() {
    let dir = healthy_dir().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let result = kernel.call_tool("capture_status", json!({}), None);
    assert_eq!(result.get("isError"), None, "{result}");
    assert_eq!(result["structuredContent"]["health"], "ok");
    assert_eq!(
        result["structuredContent"]["runs"][0]["pairs_ok"],
        2 * crate::archive::security_count() + 7
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Healthy · last capture start: complete at "),
        "{text}"
    );

    last_exit(dir.path(), ExitKind::AuthUnavailable);
    let result = kernel.call_tool("capture_status", json!({}), None);
    assert_eq!(result["structuredContent"]["health"], "failing");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "FAILING · the last capture start ended auth_unavailable (exit 75), trading day \
             2026-01-02. The token is unusable"
        ),
        "{text}"
    );
    // The tool's text, unlike the table, names when.
    assert!(
        text.contains(" · last capture start: auth_unavailable at ")
            && text.contains(" · checked "),
        "{text}"
    );
}

/// The kernel's own parser accepts the manifest, and mints the names the
/// planner instructions use.
#[test]
fn the_kernel_accepts_the_manifest() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest.json");
    let manifest = plugin::manifest::Manifest::parse(&std::fs::read_to_string(path).unwrap())
        .unwrap_or_else(|e| panic!("{e}"));
    // `parse` does not check this; `POST /api/plugins/install` does.
    assert!(
        plugin::manifest::is_installable_plugin_id(&manifest.id),
        "`{}` cannot be installed",
        manifest.id
    );
    assert_eq!(manifest.manifest_version, 5);
    let names: Vec<String> = manifest
        .exposes_tools
        .iter()
        .map(|t| plugin::results::registry_name(&manifest.id, &t.name))
        .collect();
    assert_eq!(
        names,
        [
            "plugin_archive_capture_status",
            "plugin_archive_read_at",
            "plugin_archive_capture_watch"
        ]
    );
    assert_eq!(manifest.permissions.overlays_write, ["track"]);
    let instructions = manifest.planner_instructions.as_deref().unwrap();
    for name in &names {
        assert!(instructions.contains(name.as_str()), "{name}");
    }
}

#[tokio::test]
async fn a_watching_track_gets_the_live_table_until_it_stops() {
    let dir = healthy_dir().await;
    let mut kernel = Kernel::spawn(config(dir.path()));

    let result = kernel.call_tool("capture_watch", json!({ "on": true }), Some(TRACK));
    assert_eq!(result.get("isError"), None, "{result}");
    assert_eq!(
        kernel.kv.keys().collect::<Vec<_>>(),
        [&format!("watch/{TRACK}")]
    );
    kernel.until_overlay(TRACK, |t| caption(t).starts_with("Healthy"));
    let table = kernel.overlays[&(TRACK.to_owned(), OVERLAY.to_owned())].clone();
    assert_eq!(table["rows"][0]["as_of"], "2026-01-02");
    assert_eq!(
        table["rows"][0]["ok"],
        2 * crate::archive::security_count() + 7
    );
    assert_eq!(table["rows"][0]["membership"], ".SPX.US observed");

    // A changed status reaches the Track on the poll.
    last_exit(dir.path(), ExitKind::Fatal);
    kernel.until_overlay(TRACK, |t| caption(t).starts_with("FAILING"));
    let sets = kernel.sets(TRACK);

    let result = kernel.call_tool("capture_watch", json!({ "on": false }), Some(TRACK));
    assert_eq!(result.get("isError"), None, "{result}");
    assert!(kernel.kv.is_empty() && kernel.overlays.is_empty());
    // A later change is not pushed to it: wait out one poll.
    last_exit(dir.path(), ExitKind::Complete);
    kernel.serve_for(Duration::from_secs(12));
    assert_eq!(kernel.sets(TRACK), sets);
    assert!(kernel.overlays.is_empty());
}

#[tokio::test]
async fn a_pass_pushes_only_the_tables_that_changed() {
    const OTHER: &str = "fedcba9876543210fedcba9876543210";
    let dir = healthy_dir().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    kernel.call_tool("capture_watch", json!({ "on": true }), Some(TRACK));
    kernel.until_overlay(TRACK, |t| t.is_some());
    // A second capture_watch wakes the poller for a pass over both Tracks.
    kernel.call_tool("capture_watch", json!({ "on": true }), Some(OTHER));
    kernel.until_overlay(OTHER, |t| t.is_some());
    // `capture_watch` waits for a running pass: once this returns, that pass is over.
    let never = "00000000000000000000000000000000";
    kernel.call_tool("capture_watch", json!({ "on": false }), Some(never));
    assert_eq!((kernel.sets(TRACK), kernel.sets(OTHER)), (1, 1));
    assert_eq!(
        kernel.overlays[&(TRACK.to_owned(), OVERLAY.to_owned())],
        kernel.overlays[&(OTHER.to_owned(), OVERLAY.to_owned())]
    );
}

#[tokio::test]
async fn watch_needs_a_track_and_a_flag() {
    let dir = healthy_dir().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let result = kernel.call_tool("capture_watch", json!({ "on": true }), None);
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("has none")
    );
    let result = kernel.call_tool("capture_watch", json!({}), Some(TRACK));
    assert_eq!(result["isError"], true);
    assert!(kernel.kv.is_empty());
}

#[tokio::test]
async fn a_bad_config_answers_every_tool_with_the_reason() {
    let dir = healthy_dir().await;
    for (bad, reason) in [
        (
            json!({ "data_dir": "relative/dir", "poll_seconds": 60, "stale_after_minutes": 90 }),
            "`data_dir` must be an absolute path",
        ),
        (
            json!({ "data_dir": dir.path().to_str().unwrap(), "poll_seconds": 5, "stale_after_minutes": 90 }),
            "`poll_seconds` must be an integer from 10 to 3600",
        ),
        (
            json!({ "data_dir": dir.path().to_str().unwrap(), "poll_seconds": 60 }),
            "`stale_after_minutes` must be an integer from 60 to 2880",
        ),
    ] {
        let mut kernel = Kernel::spawn(bad);
        let result = kernel.call_tool("capture_status", json!({}), None);
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(reason), "{text}");
    }
}

#[tokio::test]
async fn a_second_initialize_replaces_the_config() {
    let dir = healthy_dir().await;
    let empty = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::spawn(config(dir.path()));
    kernel.request(
        "initialize",
        json!({ "_meta": { "dev.neige/config": { "values": { "data_dir": "relative" } } } }),
    );
    let result = kernel.call_tool("capture_status", json!({}), None);
    assert_eq!(result["isError"], true, "{result}");
    kernel.request(
        "initialize",
        json!({ "_meta": { "dev.neige/config": { "values": config(empty.path()) } } }),
    );
    let result = kernel.call_tool("capture_status", json!({}), None);
    assert_eq!(result["structuredContent"]["health"], "failing", "{result}");
    assert!(
        result["structuredContent"]["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// `(name, length, modified)` of every entry of `dir` but the archive's
/// `-wal` and `-shm` sidecars, which a reader may create.
fn entries(dir: &Path) -> BTreeSet<(String, u64, std::time::SystemTime)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| {
            let name = e.file_name();
            !name.to_str().unwrap().ends_with("-wal") && !name.to_str().unwrap().ends_with("-shm")
        })
        .map(|e| {
            let meta = e.metadata().unwrap();
            let name = e.file_name().into_string().unwrap();
            (name, meta.len(), meta.modified().unwrap())
        })
        .collect()
}

#[tokio::test]
async fn reads_while_the_capture_holds_the_lock_and_changes_nothing() {
    let dir = healthy_dir().await;
    let _held = DataDirLock::try_acquire(dir.path()).unwrap();
    let oauth = dir.path().join("oauth");
    std::fs::create_dir(&oauth).unwrap();
    std::fs::write(oauth.join("token"), b"not a token").unwrap();
    let (before, before_oauth) = (entries(dir.path()), entries(&oauth));
    assert!(before.iter().any(|(name, ..)| name == "archive.lock"));

    let mut kernel = Kernel::spawn(config(dir.path()));
    let result = kernel.call_tool("capture_status", json!({}), None);
    assert_eq!(result["structuredContent"]["health"], "ok", "{result}");
    let read = json!({ "symbol": "AAPL.US", "endpoint": "consensus", "pointer": "" });
    let at = json!({ "at": "1970-01-03T00:00:00Z", "reads": [read] });
    let result = kernel.call_tool("read_at", at, None);
    assert_eq!(
        result["structuredContent"]["reads"][0]["result"], "value",
        "{result}"
    );
    kernel.call_tool("capture_watch", json!({ "on": true }), Some(TRACK));
    kernel.until_overlay(TRACK, |t| caption(t).starts_with("Healthy"));
    drop(kernel);

    assert_eq!(entries(dir.path()), before);
    assert_eq!(entries(&oauth), before_oauth);
}

/// The modules the plugin's code reaches, from `status` and `plugin`
/// through every `crate::` path outside comments: the archive's read side
/// and nothing that holds the lock, reads the token or calls a host.
#[test]
fn the_plugin_reaches_only_the_read_side() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let module = |name: &str| -> String {
        let mut files = vec![src.join(format!("{name}.rs"))];
        let mut dirs = vec![src.join(name)];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        assert!(files.len() > 1 || files[0].is_file(), "no module `{name}`");
        files
            .iter()
            .filter(|f| f.is_file())
            .flat_map(|f| {
                std::fs::read_to_string(f)
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let path = Regex::new(r"\bcrate::(\{|\w+)").unwrap();
    let forbidden = Regex::new(
        r"(?i)\blongbridge::|\buse\s+longbridge\b|oauth|\bLOCK_FILE\b|archive\.lock|\bstd::net\b|\btokio::net\b|\breqwest\b|TcpStream",
    )
    .unwrap();
    let mut reached = BTreeSet::from(["plugin".to_owned(), "status".to_owned()]);
    let mut todo: Vec<String> = reached.iter().cloned().collect();
    while let Some(name) = todo.pop() {
        let text = module(&name);
        if let Some(m) = forbidden.find(&text) {
            panic!("`{name}` names `{}`", m.as_str());
        }
        for c in path.captures_iter(&text) {
            let root = c[1].to_owned();
            assert_ne!(
                root, "{",
                "`{name}` imports `crate::{{…}}`, which this scan cannot follow"
            );
            if reached.insert(root.clone()) {
                todo.push(root);
            }
        }
    }
    assert_eq!(
        reached.into_iter().collect::<Vec<_>>(),
        [
            "catalog",
            "exit",
            "files",
            "membership",
            "plugin",
            "state",
            "status",
            "store",
            "types"
        ]
    );
    // The binary is only the plugin's entry point.
    let main = std::fs::read_to_string(src.join("plugin_main.rs")).unwrap();
    assert!(main.contains("research_archive::plugin::main()") && !forbidden.is_match(&main));
}
