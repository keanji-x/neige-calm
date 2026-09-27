//! The `initialize` model-list exchange against the fake `claude` (#1822): the argv, stdin and
//! working directory it runs with, every answer the parse can meet, and the kill-and-reap of a
//! child that hangs or floods.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::catalog_fetch::fetch;
use super::models::ClaudeCatalog;
use super::spawn::settings_json;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_fake/claude.sh"
);

struct Fake {
    dir: tempfile::TempDir,
    cwd: tempfile::TempDir,
}

impl Fake {
    fn new(catalog: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::copy(FAKE, dir.path().join("claude")).expect("copy fake");
        std::fs::write(dir.path().join("scenario"), "exit").expect("scenario");
        std::fs::write(dir.path().join("catalog"), catalog).expect("catalog");
        Self {
            dir,
            cwd: tempfile::tempdir().expect("cwd"),
        }
    }

    fn binary(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name))
            .unwrap_or_else(|_| panic!("the fake recorded {name}"))
    }

    async fn fetch(&self, timeout: Duration) -> Result<ClaudeCatalog, String> {
        let env: Vec<(String, OsString)> = vec![(
            "PATH".into(),
            std::env::var_os("PATH").expect("the test runs with a PATH"),
        )];
        fetch(&self.binary(), &env, self.cwd.path(), timeout).await
    }

    fn pid(&self) -> i32 {
        self.read("init-pid").trim().parse().expect("pid")
    }
}

fn proc_entry(pid: i32) -> PathBuf {
    Path::new("/proc").join(pid.to_string())
}

#[tokio::test]
async fn the_cli_list_is_the_catalog_and_the_exchange_is_one_initialize() {
    let fake = Fake::new("ok");
    let catalog = fake.fetch(Duration::from_secs(10)).await.expect("a list");
    assert_eq!(catalog.default.resolved_model, "claude-opus-5-5[1m]");
    assert_eq!(
        catalog.default.effort_levels,
        ["low", "medium", "high", "xhigh", "max"]
    );
    let values: Vec<&str> = catalog.models.iter().map(|m| m.value.as_str()).collect();
    assert_eq!(
        values,
        ["opus[1m]", "claude-fable-5-1[1m]", "sonnet", "haiku"]
    );
    let fable = &catalog.models[1];
    assert_eq!(fable.resolved_model, "claude-fable-5-1");
    assert_eq!(fable.display_name, "Fable");
    assert!(
        catalog.models[3].effort_levels.is_empty(),
        "Haiku declares none"
    );

    let argv: Vec<String> = fake.read("init-argv").lines().map(str::to_string).collect();
    let settings = settings_json();
    let expected: Vec<&str> = vec![
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "project",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--settings",
        &settings,
    ];
    assert_eq!(argv, expected);
    let request: serde_json::Value =
        serde_json::from_str(fake.read("init-stdin").trim()).expect("one JSON request");
    assert_eq!(request["type"], "control_request");
    assert_eq!(
        request["request"],
        serde_json::json!({"subtype": "initialize"})
    );
    assert_eq!(
        Path::new(fake.read("init-cwd").trim()),
        fake.cwd.path().canonicalize().unwrap()
    );
}

#[tokio::test]
async fn a_malformed_or_empty_answer_is_no_list() {
    for answer in ["malformed", "empty-list", "empty"] {
        let reason = Fake::new(answer)
            .fetch(Duration::from_secs(10))
            .await
            .expect_err(answer);
        assert!(
            reason.contains("printed no usable model list"),
            "{answer}: {reason}"
        );
        assert!(
            !reason.contains("owner@example.invalid"),
            "{answer}: {reason}"
        );
    }
}

/// Current-thread on purpose: between the return and the `/proc` read nothing else runs, so an
/// unreaped child is still visible (as a zombie) here.
#[tokio::test(flavor = "current_thread")]
async fn a_hanging_initialize_is_killed_and_reaped_on_timeout() {
    let fake = Fake::new("hang");
    let started = std::time::Instant::now();
    let reason = fake
        .fetch(Duration::from_millis(500))
        .await
        .expect_err("a hung child is no list");
    assert!(reason.contains("did not answer within"), "{reason}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "bounded by the timeout"
    );
    let pid = fake.pid();
    assert!(
        !proc_entry(pid).exists(),
        "the hung child {pid} must be killed and reaped before the fetch returns"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_flooding_initialize_is_refused_killed_and_reaped() {
    let fake = Fake::new("flood");
    let reason = fake
        .fetch(Duration::from_secs(10))
        .await
        .expect_err("an over-long answer is refused");
    assert!(reason.contains("printed more than"), "{reason}");
    let pid = fake.pid();
    assert!(
        !proc_entry(pid).exists(),
        "the flooding child {pid} must be killed and reaped before the fetch returns"
    );
}
