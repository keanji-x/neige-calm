//! I11 for the capture job, and the `notify` command: no token, refresh
//! token or account string reaches the binary's output, `last_exit.json`,
//! the alert message or the archive, through a run with an auth failure,
//! throttles, transient and permanent failures that quote the account.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use research_archive::clock::{Clock, SystemClock};
use research_archive::state::{ExitKind, LastExit};

use crate::archive::Knobs;
use crate::capture_proc::{Proc, code, text};
use crate::fake::{FakeServer, Reply, Seen};

const ACCESS: &str = "CANARY-CAP-ACCESS-6b2e";
const REFRESH: &str = "CANARY-CAP-REFRESH-3f90";
const ACCOUNT: &str = "CANARY-CAP-ACCOUNT-77120045";
const CANARIES: [&str; 3] = [ACCESS, REFRESH, ACCOUNT];
const UNIT: &str = "research-archive-capture.service";

fn assert_clean(what: &str, bytes: &[u8]) {
    let haystack = String::from_utf8_lossy(bytes);
    for canary in CANARIES {
        assert!(!haystack.contains(canary), "{what} contains {canary}");
    }
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// A notify command that appends its message to `<out>/messages`, counts
/// calls in `<out>/count`, saves its environment to `<out>/env`, and exits
/// with the code in `<out>/exit` (0 when absent).
fn notify_script(root: &Path) -> (PathBuf, PathBuf) {
    let out = root.join("notify-out");
    std::fs::create_dir(&out).unwrap();
    let script = root.join("notify.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nout=\"$1\"\nprintf '%s\\n' \"$2\" >> \"$out/messages\"\n\
         printf 'x' >> \"$out/count\"\n/usr/bin/env > \"$out/env\"\n\
         if [ -f \"$out/exit\" ]; then exit \"$(cat \"$out/exit\")\"; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, out)
}

fn knobs(script: &Path, out: &Path) -> Knobs {
    Knobs {
        attempt_cap: 2,
        notify_command: vec![script.display().to_string(), out.display().to_string()],
        ..Knobs::default()
    }
}

async fn notify(p: &Proc) -> Output {
    let mut command = p.command(&["notify", "--failed-unit", UNIT]);
    command
        .env("PATH", "/usr/bin:/bin")
        .env("FOO", "not-allowlisted");
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i11_capture_output_exit_state_alert_and_archive_hold_no_secret() {
    let stage = Arc::new(AtomicUsize::new(0));
    let throttles = Arc::new(AtomicUsize::new(0));
    let (s, th) = (stage.clone(), throttles.clone());
    let server = FakeServer::start(move |seen: &Seen| match seen.path.as_str() {
        "/v1/quote/valuation" if s.load(Ordering::SeqCst) == 0 => {
            Reply::new(401, format!(r#"{{"error":"{ACCOUNT}"}}"#))
        }
        "/v1/quote/forecast-eps" if th.fetch_add(1, Ordering::SeqCst) < 2 => Reply::new(
            200,
            format!(r#"{{"code":429002,"message":"member {ACCOUNT} retry after: 0.1s"}}"#),
        ),
        "/v1/quote/institution-ratings" => {
            Reply::new(500, format!("<html>{ACCOUNT}</html>")).header("x-trace-id", ACCOUNT)
        }
        "/v1/quote/institution-ratings/detail" => Reply::new(
            200,
            format!(r#"{{"code":2601400,"message":"no data for {ACCOUNT}","data":null}}"#),
        ),
        _ => Reply::data(r#"{"eps":1.5}"#),
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let (script, out) = notify_script(root.path());
    let p = Proc::new(&server.url, knobs(&script, &out));
    let mut outputs = Vec::new();
    let import = p
        .import(
            &format!("ap_m_{ACCESS}.{ACCOUNT}"),
            Some(REFRESH),
            4_102_444_800,
        )
        .await;
    assert_eq!(code(&import), 0, "{}", text(&import.stderr));
    outputs.push(("import", import));

    // A request is refused for auth: the run stops open, the job exits 75,
    // and the alert goes out.
    let first = p.capture().await;
    assert_eq!(code(&first), 75, "{}", text(&first.stderr));
    outputs.push(("capture 1", first));
    let state_auth = read(&p.archive.data_dir.join("last_exit.json"));
    let alert = notify(&p).await;
    assert_eq!(code(&alert), 0, "{}", text(&alert.stderr));
    outputs.push(("notify", alert));
    let message = read(&out.join("messages"));
    assert!(
        message.contains("failed: auth_unavailable (exit 75)"),
        "{message}"
    );

    // The next start resumes: throttles clear, the rest fail to the cap.
    stage.store(1, Ordering::SeqCst);
    let second = p.capture().await;
    assert_eq!(code(&second), 0, "{}", text(&second.stderr));
    outputs.push(("capture 2", second));
    let state = p.archive.last_exit();
    assert_eq!(state["kind"], "complete_with_failures");

    // The canaries did flow.
    let bearer = format!("Bearer ap_m_{ACCESS}.{ACCOUNT}");
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.header("authorization") == Some(&*bearer))
    );
    assert!(read(&p.token_path()).contains(REFRESH));

    for (what, output) in &outputs {
        assert_clean(&format!("stdout of {what}"), &output.stdout);
        assert_clean(&format!("stderr of {what}"), &output.stderr);
    }
    assert_clean(
        "last_exit.json after the auth failure",
        state_auth.as_bytes(),
    );
    assert_clean("the alert message", message.as_bytes());
    for path in files(root.path())
        .into_iter()
        .chain(files(p.archive.root.path()))
    {
        if path == p.token_path() || path.ends_with("sdk-token.json") {
            continue;
        }
        assert_clean(&path.display().to_string(), &std::fs::read(&path).unwrap());
    }
}

/// The alert: the message names the failure kind; the same kind is sent
/// once until six hours pass; a new kind is sent at once; a stale or
/// successful exit state is `unknown`; a failing command is `FATAL` and
/// not recorded as sent; the command sees only the allowlisted variables.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_dedupes_by_kind_and_passes_only_allowlisted_variables() {
    let server = FakeServer::start(|_| Reply::data("{}")).await;
    let root = tempfile::tempdir().unwrap();
    let (script, out) = notify_script(root.path());
    let p = Proc::new(&server.url, knobs(&script, &out));
    std::fs::create_dir_all(&p.archive.data_dir).unwrap();
    let data_dir = &p.archive.data_dir;
    let state = |kind: ExitKind, age_ms: i64| LastExit {
        code: kind.code(),
        kind,
        at: SystemClock.now().0 - age_ms,
        as_of: Some(p.as_of.clone()),
        failure: (kind == ExitKind::Partial).then(|| "envelope:401004".to_owned()),
    };
    let count = || read(&out.join("count")).len();

    state(ExitKind::AuthUnavailable, 1_000).write(data_dir);
    assert_eq!(code(&notify(&p).await), 0);
    assert_eq!(count(), 1);
    let messages = read(&out.join("messages"));
    assert!(
        messages.contains(&format!(
            "research-archive: {UNIT} failed: auth_unavailable (exit 75)"
        )),
        "{messages}"
    );
    let env = read(&out.join("env"));
    let names: Vec<&str> = env.lines().filter_map(|l| l.split('=').next()).collect();
    assert!(names.contains(&"PATH"), "{env}");
    // The job's environment has ALL_PROXY and FOO; neither is allowlisted.
    assert!(!env.contains("FOO") && !env.contains("ALL_PROXY"), "{env}");
    for name in names {
        assert!(
            research_archive::notify::ENV_ALLOWLIST.contains(&name) || name == "PWD",
            "{name} reached the notify command"
        );
    }

    // The same kind again: not repeated.
    assert_eq!(code(&notify(&p).await), 0);
    assert_eq!(count(), 1);
    // A new kind: sent, with the most frequent failure.
    state(ExitKind::Partial, 1_000).write(data_dir);
    assert_eq!(code(&notify(&p).await), 0);
    assert_eq!(count(), 2);
    assert!(read(&out.join("messages")).contains("most frequent failure envelope:401004"));
    // Six hours after the last send: repeated.
    let sent = p.archive.data_dir.join("notify_state.json");
    let mut last: serde_json::Value = serde_json::from_str(&read(&sent)).unwrap();
    last["at"] = (SystemClock.now().0 - 6 * 3_600_000 - 1).into();
    std::fs::write(&sent, last.to_string()).unwrap();
    assert_eq!(code(&notify(&p).await), 0);
    assert_eq!(count(), 3);
    // A stale exit state is `unknown`.
    state(ExitKind::AuthUnavailable, 3_600_000).write(data_dir);
    assert_eq!(code(&notify(&p).await), 0);
    assert_eq!(count(), 4);
    assert!(read(&out.join("messages")).ends_with("failed: unknown (it recorded no exit state)\n"));
    // A failing command: FATAL, and the kind is not recorded as sent.
    std::fs::write(out.join("exit"), "1").unwrap();
    state(ExitKind::Fatal, 1_000).write(data_dir);
    assert_eq!(code(&notify(&p).await), 70);
    assert_eq!(count(), 5);
    let last: serde_json::Value = serde_json::from_str(&read(&sent)).unwrap();
    assert_eq!(last["kind"], "unknown");
}
