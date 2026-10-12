//! The `research-archive` binary as a subprocess ([`crate::cli_env`]): the
//! environment guard, the lock, and the `auth` commands. No command here
//! reaches the SDK's network code: `import` and `status` never build an
//! `OAuth`, and `login` is killed while it waits for the browser callback.

use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use research_archive::lock::DataDirLock;

use crate::cli_env::{ACCESS, CLIENT, Env, REFRESH, text};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn an_sdk_variable_in_the_environment_is_fatal() {
    let env = Env::new();
    let from = env.sdk_token("token", CLIENT, None);
    let out = env
        .command(&["auth", "import", "--from", from.to_str().unwrap()])
        .env("LONGBRIDGE_HTTP_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(70), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("LONGBRIDGE_HTTP_URL"), "{stderr}");
    assert!(
        !stderr.contains("127.0.0.1:1"),
        "values are not echoed: {stderr}"
    );
    assert!(!env.data_dir.exists(), "nothing was written");
}

#[test]
fn a_dotenv_above_the_working_directory_is_fatal() {
    let env = Env::new();
    let nested = env.cwd.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        env.cwd.join(".env"),
        "LONGBRIDGE_HTTP_URL=http://127.0.0.1:1\n",
    )
    .unwrap();
    let out = env
        .command(&["auth", "status"])
        .current_dir(&nested)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(70), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains(".env"));
}

#[test]
fn a_clean_environment_proceeds_to_the_command() {
    let env = Env::new();
    let out = env.run(&["auth", "status"]);
    assert_eq!(out.status.code(), Some(75), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("no token stored"));

    let from = env.sdk_token("token", CLIENT, Some(REFRESH));
    let out = env.run(&["auth", "import", "--from", from.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = env.run(&["auth", "status"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "expires_at=4102444800 (2100-01-01T00:00:00+00:00) refresh_token=present\n"
    );
}

#[test]
fn import_stores_a_0600_copy_and_leaves_the_source_alone() {
    let env = Env::new();
    let from = env.sdk_token(CLIENT, CLIENT, Some(REFRESH));
    fs::set_permissions(&from, fs::Permissions::from_mode(0o644)).unwrap();
    let before = fs::read(&from).unwrap();

    let out = env.run(&["auth", "import", "--from", from.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "imported: expires_at=4102444800 (2100-01-01T00:00:00+00:00) refresh_token=present\n"
    );
    assert_eq!(mode(&env.token_path()), 0o600);
    assert_eq!(mode(&env.data_dir.join("oauth")), 0o700);
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(env.token_path()).unwrap()).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(stored, original);
    assert_eq!(fs::read(&from).unwrap(), before);
    assert_eq!(mode(&from), 0o644);
}

#[test]
fn import_refuses_another_client_or_a_malformed_file_and_writes_nothing() {
    let env = Env::new();
    let other = env.sdk_token("other", "other-client", None);
    let out = env.run(&["auth", "import", "--from", other.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(70));
    assert!(text(&out.stderr).contains("another OAuth client id"));
    assert!(!env.token_path().exists());

    // serde would quote this mistyped value in its own message.
    let bad = env.root.path().join("bad");
    fs::write(
        &bad,
        format!(r#"{{"client_id":"{CLIENT}","access_token":"{ACCESS}","refresh_token":null,"expires_at":"{REFRESH}"}}"#),
    )
    .unwrap();
    let out = env.run(&["auth", "import", "--from", bad.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(70));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("is not a token: data error"), "{stderr}");
    assert!(!stderr.contains("CANARY"), "{stderr}");
    assert!(!env.token_path().exists());
}

#[test]
fn import_and_login_exit_locked_while_the_lock_is_held() {
    let env = Env::new();
    let first = env.sdk_token("first", CLIENT, Some(REFRESH));
    let out = env.run(&["auth", "import", "--from", first.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stored = fs::read(env.token_path()).unwrap();

    let held = DataDirLock::try_acquire(&env.data_dir).unwrap();
    let second = env.sdk_token("second", CLIENT, None);
    for args in [
        &[
            "auth",
            "import",
            "--force",
            "--from",
            second.to_str().unwrap(),
        ][..],
        &["auth", "login"][..],
    ] {
        let out = env.run(args);
        assert_eq!(
            out.status.code(),
            Some(4),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(
            text(&out.stderr).contains("held by another process"),
            "{args:?}"
        );
        assert!(text(&out.stdout).is_empty(), "{args:?}");
        assert_eq!(fs::read(env.token_path()).unwrap(), stored, "{args:?}");
    }
    drop(held);
}

/// B1: `import --from` refuses a FIFO, a device or a directory before it
/// takes the lock, so it can neither block on the read nor hold the lock.
#[test]
fn import_refuses_a_non_regular_source_before_the_lock() {
    let env = Env::new();
    let fifo = env.root.path().join("fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let held = DataDirLock::try_acquire(&env.data_dir).unwrap();
    for from in [fifo.as_path(), Path::new("/dev/zero"), env.root.path()] {
        let out = env.run(&["auth", "import", "--from", from.to_str().unwrap()]);
        assert_eq!(
            out.status.code(),
            Some(70),
            "{from:?}: {}",
            text(&out.stderr)
        );
        assert!(text(&out.stderr).contains("not a regular file"), "{from:?}");
    }
    drop(held);
    assert!(!env.token_path().exists());
}

/// B2: a token whose contents cannot be sent is refused on import, and a
/// stored one makes `status` exit 75. Neither echoes the value.
#[test]
fn unsendable_token_contents_are_refused_by_import_and_status() {
    let env = Env::new();
    for (access, refresh, field) in [
        (
            "ap_CANARY\nline",
            serde_json::json!(REFRESH),
            "access_token",
        ),
        (ACCESS, serde_json::json!(""), "refresh_token"),
    ] {
        let json = serde_json::json!({
            "client_id": CLIENT,
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": 4_102_444_800u64,
        })
        .to_string();
        let from = env.root.path().join("bad");
        fs::write(&from, &json).unwrap();
        let out = env.run(&[
            "auth",
            "import",
            "--force",
            "--from",
            from.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(70), "{field}");
        let stderr = text(&out.stderr);
        assert!(stderr.contains(&format!("`{field}` is")), "{stderr}");
        assert!(!stderr.contains("CANARY"), "{stderr}");
        assert!(!env.token_path().exists());

        fs::create_dir_all(env.token_path().parent().unwrap()).unwrap();
        fs::write(env.token_path(), &json).unwrap();
        let out = env.run(&["auth", "status"]);
        assert_eq!(out.status.code(), Some(75), "{field}");
        assert!(text(&out.stdout).is_empty());
        assert!(!text(&out.stderr).contains("CANARY"));
        fs::remove_file(env.token_path()).unwrap();
    }
}

/// `status` exits 75 for a token the SDK cannot use without a browser
/// login; `import` stores one only with `--force`.
#[test]
fn an_unusable_token_is_refused_by_import_and_reported_by_status() {
    let env = Env::new();
    let expired = env.root.path().join("expired");
    fs::write(
        &expired,
        serde_json::json!({
            "client_id": CLIENT,
            "access_token": ACCESS,
            "refresh_token": null,
            "expires_at": 1_000_000_000u64,
        })
        .to_string(),
    )
    .unwrap();
    let import = ["auth", "import", "--from", expired.to_str().unwrap()];
    let out = env.run(&import);
    assert_eq!(out.status.code(), Some(70));
    assert!(
        text(&out.stderr).contains("--force"),
        "{}",
        text(&out.stderr)
    );
    assert!(!env.token_path().exists());

    let forced = [&import[..], &["--force"]].concat();
    let out = env.run(&forced);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = env.run(&["auth", "status"]);
    assert_eq!(out.status.code(), Some(75));
    assert!(text(&out.stdout).contains("refresh_token=absent"));
    assert!(text(&out.stderr).contains("auth login"));
}

/// `auth login` shows the authorization URL on stderr, for the region's
/// OAuth host and the registered callback port, then waits for the browser.
/// It is killed there: nothing has been sent anywhere, and nothing stored.
#[test]
fn login_prints_the_authorization_url_for_the_registered_port() {
    let env = Env::new();
    let mut child = env
        .command(&["auth", "login"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if line.starts_with("https://") {
                let _ = tx.send(line);
                break;
            }
        }
    });
    let url = rx.recv_timeout(Duration::from_secs(60));
    child.kill().unwrap();
    child.wait().unwrap();
    let url = url.expect("login printed no URL");
    assert!(
        url.starts_with("https://openapi.longbridge.cn/oauth2/authorize?"),
        "{url}"
    );
    assert!(url.contains(&format!("client_id={CLIENT}")), "{url}");
    let redirect = format!(
        "redirect_uri=http%3A%2F%2Flocalhost%3A{}%2Fcallback",
        env.callback_port
    );
    assert!(url.contains(&redirect), "{url}");
    assert!(!env.token_path().exists());
}
