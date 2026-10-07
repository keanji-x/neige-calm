//! #2387 — the gate verdict and its log tail. A failure belongs to the step the wrapper last
//! started, which the wrapper records in the step file; text a step prints never moves it.
use super::*;

const LOG_TAIL_BYTES: usize = 8 * 1024;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gate-{name}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn evidence(dir: &Path, steps: &[(&str, &str)]) -> GateEvidence {
    GateEvidence {
        log_path: dir.join("wrapper.log"),
        step_path: dir.join("wrapper.step"),
        steps: steps
            .iter()
            .map(|(name, cmd)| GateStep {
                name: (*name).into(),
                cmd: (*cmd).into(),
            })
            .collect(),
    }
}

/// Run the real wrapper under `/bin/sh` the way `spawn_held` does: log on stdout and stderr, both
/// evidence paths in the environment, released by the go-token. Returns its exit code.
async fn run_wrapper(dir: &Path, evidence: &GateEvidence) -> i32 {
    let script_path = dir.join("wrapper.sh");
    std::fs::write(&script_path, render_gate_wrapper(&evidence.steps)).unwrap();
    let log_file = std::fs::File::create(&evidence.log_path).unwrap();
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg(&script_path)
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(log_file.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log_file))
        .env("NEIGE_GATE_EXIT_PATH", dir.join("wrapper.exit"))
        .env("NEIGE_GATE_STEP_PATH", &evidence.step_path)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"go\n").await.unwrap();
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(60), child.wait())
        .await
        .expect("the wrapper finishes")
        .unwrap();
    status.code().expect("the wrapper exits")
}

#[test]
fn verdict_classification() {
    let dir = scratch("verdict");
    let evidence = evidence(&dir, &[("fmt", "true"), ("test", "false")]);
    std::fs::write(&evidence.log_path, "ok\n").unwrap();

    std::fs::write(&evidence.step_path, "2\n").unwrap();
    let v = verdict_from_exit_code(0, &evidence, 1);
    assert!(v.passed);
    assert_eq!(v.status_detail, None);
    assert_eq!(v.failing_step, None);

    let v = verdict_from_exit_code(101, &evidence, 2);
    assert!(!v.passed);
    assert_eq!(v.status_detail.as_deref(), Some("gate-red"));
    assert_eq!(v.failing_step.as_deref(), Some("test"));
    assert_eq!(v.exit_code, Some(101));
    assert_eq!(v.attempt, 2);

    let v = timeout_verdict(&evidence, 1, 7);
    assert_eq!(v.status_detail.as_deref(), Some("gate-timeout"));
    assert_eq!(v.failing_step.as_deref(), Some("test"), "the running step");

    // A record that names no declared step attributes nothing.
    for record in ["3\n", "0\n", "two\n", ""] {
        std::fs::write(&evidence.step_path, record).unwrap();
        let v = verdict_from_exit_code(1, &evidence, 1);
        assert_eq!(v.status_detail.as_deref(), Some("gate-infra"), "{record:?}");
        assert_eq!(v.failing_step, None, "{record:?}");
    }

    std::fs::remove_file(&evidence.step_path).unwrap();
    std::fs::write(&evidence.log_path, "").unwrap();
    let v = verdict_from_exit_code(75, &evidence, 1);
    assert_eq!(v.status_detail.as_deref(), Some("gate-infra"));
    assert_eq!(v.failing_step, None);
    let v = timeout_verdict(&evidence, 1, 7);
    assert!(v.log_tail.contains("timed out after 7s"));
    assert_eq!(v.failing_step, None);

    std::fs::remove_dir_all(&dir).ok();
}

/// The #2387 case: a red step that prints far more than the tail is still `gate-red` at that step.
#[tokio::test]
async fn a_red_step_with_long_output_keeps_its_attribution() {
    let dir = scratch("long-red");
    let evidence = evidence(
        &dir,
        &[
            ("build", "echo built"),
            (
                "frontend contracts",
                "i=0; while [ $i -lt 4000 ]; do \
                 printf '\\033[31m%s\\033[0m\\n' \"$(printf '%080d' $i)\"; i=$((i+1)); \
                 done; echo '5 failed'; exit 3",
            ),
            ("never", "echo unreachable"),
        ],
    );
    assert_eq!(run_wrapper(&dir, &evidence).await, 3);
    let log_len = std::fs::metadata(&evidence.log_path).unwrap().len();
    assert!(log_len > 300 * 1024, "the red step printed {log_len} bytes");

    let verdict = verdict_from_exit_code(3, &evidence, 1);
    assert_eq!(verdict.status_detail.as_deref(), Some("gate-red"));
    assert_eq!(verdict.failing_step.as_deref(), Some("frontend contracts"));
    assert_eq!(verdict.exit_code, Some(3));
    assert!(verdict.log_tail.ends_with("5 failed\n"), "{}", verdict.log_tail);
    assert!(!verdict.log_tail.contains('\u{1b}'), "no escape sequences");
    std::fs::remove_dir_all(&dir).ok();
}

/// A step whose output ends without a newline, then a failing step: the failure is the second
/// step's, though its `::gate-step` line in the log is glued to the first step's output.
#[tokio::test]
async fn output_without_a_final_newline_does_not_move_the_failure() {
    let dir = scratch("no-newline");
    let evidence = evidence(
        &dir,
        &[
            ("build", "printf 'progress 100%%'"),
            ("test", "echo boom; exit 4"),
        ],
    );
    assert_eq!(run_wrapper(&dir, &evidence).await, 4);
    let log = std::fs::read_to_string(&evidence.log_path).unwrap();
    assert!(log.contains("progress 100%::gate-step test"), "{log}");

    let verdict = verdict_from_exit_code(4, &evidence, 1);
    assert_eq!(verdict.status_detail.as_deref(), Some("gate-red"));
    assert_eq!(verdict.failing_step.as_deref(), Some("test"));
    std::fs::remove_dir_all(&dir).ok();
}

/// A step that prints another step's `::gate-step` line does not take over the failure.
#[tokio::test]
async fn a_printed_step_line_cannot_claim_the_failure() {
    let dir = scratch("printed-step");
    let evidence = evidence(
        &dir,
        &[
            ("build", "true"),
            ("test", "echo '::gate-step build'; exit 5"),
        ],
    );
    assert_eq!(run_wrapper(&dir, &evidence).await, 5);
    let verdict = verdict_from_exit_code(5, &evidence, 1);
    assert_eq!(verdict.failing_step.as_deref(), Some("test"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn exit_file_parse_states() {
    let dir = scratch("exit");
    let path = dir.join("a.exit");
    assert_eq!(read_exit_file(&path), Ok(None), "absent");
    std::fs::write(&path, "3\n").unwrap();
    assert_eq!(read_exit_file(&path), Ok(Some(3)));
    std::fs::write(&path, "not-a-code").unwrap();
    assert_eq!(read_exit_file(&path), Err(()), "foreign artifact");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn log_tail_caps_at_8kib() {
    let dir = scratch("tail");
    let log = dir.join("big.log");
    let mut content = "x".repeat(200 * 1024);
    content.push_str("\ntail-end\n");
    std::fs::write(&log, &content).unwrap();
    let tail = read_log_tail(&log);
    assert!(tail.len() <= LOG_TAIL_BYTES);
    assert!(tail.ends_with("tail-end\n"));
    assert_eq!(read_log_tail(&dir.join("absent.log")), "");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_log_tail_drops_terminal_escapes() {
    let dir = scratch("ansi");
    let log = dir.join("ansi.log");
    for (raw, text) in [
        (
            "\u{1b}[1;31mFAIL\u{1b}[0m a.test.ts\n\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{1b}\\\n\u{1b}(Bdone\u{1b}=\n",
            "FAIL a.test.ts\nlink\ndone\n",
        ),
        // An unterminated OSC ends at its line; the summary after it stays.
        (
            "FAIL a.test.ts\n\u{1b}]8;;http://x\nTests  5 failed | 4898 passed\n",
            "FAIL a.test.ts\n\nTests  5 failed | 4898 passed\n",
        ),
        // A CSI broken by a newline ends there; the next line stays whole.
        ("\u{1b}[\nFAIL x\n", "\nFAIL x\n"),
        // A tail cut inside a sequence, and a trailing lone escape.
        ("31mred\u{1b}[0m\n\u{1b}", "31mred\n"),
    ] {
        std::fs::write(&log, raw).unwrap();
        assert_eq!(read_log_tail(&log), text, "{raw:?}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Against a REAL `/bin/sh`: dropping the only write end of the stdin pipe makes `read -r _go` hit
/// EOF and the child exit 75 having executed nothing, recording no step.
#[tokio::test]
async fn wrapper_handshake_eof_exits_75_having_run_nothing() {
    let dir = scratch("handshake");
    let marker = dir.join("step-ran");
    let script_path = dir.join("wrapper.sh");
    let exit_path = dir.join("wrapper.exit");
    let evidence = evidence(&dir, &[("touch", &format!("touch {}", marker.display()))]);
    std::fs::write(&script_path, render_gate_wrapper(&evidence.steps)).unwrap();
    std::fs::write(&evidence.log_path, "").unwrap();

    let mut child = tokio::process::Command::new("/bin/sh")
        .arg(&script_path)
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("NEIGE_GATE_EXIT_PATH", &exit_path)
        .env("NEIGE_GATE_STEP_PATH", &evidence.step_path)
        .spawn()
        .unwrap();
    // Kernel-death stand-in: drop the held stdin WITHOUT writing the go-token.
    drop(child.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("EOF must release the held wrapper promptly")
        .unwrap();
    assert_eq!(status.code(), Some(75), "{status:?}");
    assert!(!marker.exists(), "no gate step may run before release");
    assert!(
        !exit_path.exists(),
        "the handshake exit path bypasses neige_gate_finish"
    );
    assert!(!evidence.step_path.exists(), "no step started");

    let verdict = verdict_from_exit_code(75, &evidence, 1);
    assert_eq!(verdict.status_detail.as_deref(), Some("gate-infra"));
    assert_eq!(verdict.failing_step, None);

    std::fs::remove_dir_all(&dir).ok();
}
