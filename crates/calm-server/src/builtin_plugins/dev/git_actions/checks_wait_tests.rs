//! The wait loop runs against a controlled reader; tests/cases/forge_pr_checks.rs drives
//! the complete production GraphQL reader, shared fold and wait through the kernel.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

const FAKE_GH: &str = concat!(
    "#!/bin/sh\n",
    "printf '%s\\n' \"$*\" >> \"$GH_FAKE_DIR/calls\"\n",
    "n=$(($(wc -l < \"$GH_FAKE_DIR/calls\")))\n",
    "[ \"$n\" -le \"$GH_FAKE_LAST\" ] || n=$GH_FAKE_LAST\n",
    "[ ! -e \"$GH_FAKE_DIR/fail.$n\" ] || exit 1\n",
    "cat \"$GH_FAKE_DIR/read.$n\"\n",
);

/// Runs the lowered wait with a zero poll interval. The fake `gh` answers its n-th call with
/// `reads[n-1]` (the last one repeats), and a `None` read fails that call. Returns the wait's
/// stdout and the number of `gh` calls; a wait that never ends fails the test.
fn run_wait(reads: &[Option<&str>]) -> (String, usize) {
    let dir = tempfile::tempdir().expect("tempdir");
    let gh = dir.path().join("gh");
    std::fs::write(&gh, FAKE_GH).expect("write fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
    for (index, read) in reads.iter().enumerate() {
        let n = index + 1;
        match read {
            Some(stdout) => write_read(dir.path(), &format!("read.{n}"), stdout),
            None => write_read(dir.path(), &format!("fail.{n}"), ""),
        }
    }
    let payload = lower("gh.pr.checks", &json!({ "repo": "owner/repo", "pr": 42 }))
        .expect("lower gh pr checks");
    let mut argv: Vec<String> = serde_json::from_value(payload["argv"].clone()).expect("argv");
    assert_eq!(argv[6], PR_CHECKS_POLL_SECS.to_string(), "{argv:?}");
    argv[6] = "0".into();
    // Isolate only the polling state machine here. Integration tests execute this reader's
    // real lowering, pagination validation and classifier rather than duplicating policy.
    argv[8] = "gh".into();
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env("PATH", &path)
        .env("GH_FAKE_DIR", dir.path())
        .env("GH_FAKE_LAST", reads.len().to_string())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run the checks wait");
    let started = Instant::now();
    while child.try_wait().expect("poll the checks wait").is_none() {
        if started.elapsed() > Duration::from_secs(10) {
            child.kill().expect("kill the checks wait");
            child.wait().expect("reap the checks wait");
            panic!("the checks wait never ended");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("checks wait output");
    assert!(output.status.success(), "{output:?}");
    let calls = std::fs::read_to_string(dir.path().join("calls")).expect("fake gh calls");
    (
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        calls.lines().count(),
    )
}

fn write_read(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fake gh read");
}

fn pretty(conclusion: &str, head_sha: Value) -> (String, Value) {
    let value = json!({ "conclusion": conclusion, "mergeable": "mergeable", "head_sha": head_sha });
    (
        serde_json::to_string_pretty(&value).expect("pretty json"),
        value,
    )
}

/// The verdict is jq's raw first line, so the wait settles and tracks the head whatever the
/// layout of the JSON after it, and prints only that JSON.
#[test]
fn gh_pr_checks_wait_reads_the_verdict_line_not_the_json_text() {
    let (failed, failed_value) = pretty("failure", json!("abc"));
    let (stdout, calls) = run_wait(&[Some(&format!("settle abc\n{failed}\n"))]);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).expect("the wait prints only JSON"),
        failed_value
    );
    assert_eq!(calls, 1);

    // A failed read is retried and an empty head neither sets the baseline nor counts as a move;
    // the change from `abc` to `def` ends the wait.
    let (no_head, _) = pretty("pending", Value::Null);
    let (pending, _) = pretty("pending", json!("abc"));
    let (moved, moved_value) = pretty("pending", json!("def"));
    let (stdout, calls) = run_wait(&[
        None,
        Some(&format!("wait \n{no_head}\n")),
        Some(&format!("wait abc\n{pending}\n")),
        Some(&format!("wait \n{no_head}\n")),
        Some(&format!("wait def\n{moved}\n")),
    ]);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).expect("the wait prints only JSON"),
        moved_value
    );
    assert_eq!(calls, 5);
}

/// A failed `gh` read does not end the wait (it would wake no one); the next read does.
#[test]
fn gh_pr_checks_wait_retries_a_failed_read() {
    let settled = r#"{"conclusion":"success","mergeable":"mergeable","head_sha":"abc"}"#;
    let (stdout, calls) = run_wait(&[None, Some(&format!("settle abc\n{settled}\n"))]);
    assert_eq!(stdout, format!("{settled}\n"));
    assert_eq!(calls, 2);
}
