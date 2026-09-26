//! `auth status --json` against the fake `claude` (#1817): every answer the parse can meet, and
//! the kill-and-reap of a child that hangs or floods.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::auth_status::logged_in;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_fake/claude.sh"
);

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new(auth: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::copy(FAKE, dir.path().join("claude")).expect("copy fake");
        std::fs::write(dir.path().join("scenario"), "exit").expect("scenario");
        std::fs::write(dir.path().join("auth"), auth).expect("auth");
        Self { dir }
    }

    fn binary(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    fn env() -> Vec<(String, OsString)> {
        vec![(
            "PATH".into(),
            std::env::var_os("PATH").expect("the test runs with a PATH"),
        )]
    }

    async fn ask(&self, timeout: Duration) -> Result<bool, String> {
        logged_in(&self.binary(), &Self::env(), timeout).await
    }

    fn pid(&self) -> i32 {
        std::fs::read_to_string(self.dir.path().join("auth-pid"))
            .expect("the fake recorded its pid")
            .trim()
            .parse()
            .expect("pid")
    }
}

fn proc_entry(pid: i32) -> PathBuf {
    Path::new("/proc").join(pid.to_string())
}

#[tokio::test]
async fn logged_in_reads_the_boolean_and_nothing_else() {
    assert_eq!(
        Fake::new("logged-in").ask(Duration::from_secs(10)).await,
        Ok(true)
    );
    // `logged-out` also exits 1: the answer is the boolean, not the exit status.
    assert_eq!(
        Fake::new("logged-out").ask(Duration::from_secs(10)).await,
        Ok(false)
    );
}

#[tokio::test]
async fn an_answer_without_a_boolean_logged_in_is_no_answer() {
    for answer in ["no-field", "not-bool", "not-json"] {
        let reason = Fake::new(answer)
            .ask(Duration::from_secs(10))
            .await
            .expect_err(answer);
        assert!(
            reason.contains("printed no boolean `loggedIn`"),
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
async fn a_hanging_auth_status_is_killed_and_reaped_on_timeout() {
    let fake = Fake::new("hang");
    let started = std::time::Instant::now();
    let reason = fake
        .ask(Duration::from_millis(500))
        .await
        .expect_err("a hung child is no answer");
    assert!(reason.contains("did not answer within"), "{reason}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "bounded by the timeout"
    );
    let pid = fake.pid();
    assert!(
        !proc_entry(pid).exists(),
        "the hung child {pid} must be killed and reaped before the check returns"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_flooding_auth_status_is_refused_killed_and_reaped() {
    let fake = Fake::new("flood");
    let reason = fake
        .ask(Duration::from_secs(10))
        .await
        .expect_err("an over-long answer is refused");
    assert!(reason.contains("printed more than"), "{reason}");
    let pid = fake.pid();
    assert!(
        !proc_entry(pid).exists(),
        "the flooding child {pid} must be killed and reaped before the check returns"
    );
}

#[tokio::test]
async fn a_missing_binary_is_a_reason() {
    let reason = logged_in(
        Path::new("/nonexistent/claude-1817"),
        &Fake::env(),
        Duration::from_secs(10),
    )
    .await
    .expect_err("no binary, no answer");
    assert!(reason.contains("could not start"), "{reason}");
}
