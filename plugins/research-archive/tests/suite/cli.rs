//! The `research-archive` binary as a subprocess: the environment guard, the
//! lock, and the `auth` commands. Every run gets an empty environment except
//! proxies that point at a closed local port, so even a mistaken network
//! call could not leave the machine. No command here reaches the SDK's
//! network code: `import` and `status` never build an `OAuth`, and `login`
//! is killed while it waits for the browser callback.

use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::Duration,
};

use research_archive::lock::DataDirLock;

pub const CLIENT: &str = "test-client";
pub const ACCESS: &str = "ap_m_CANARY-ACCESS-7f3a9c";
pub const REFRESH: &str = "CANARY-REFRESH-51d0e2";

/// A private directory with a config, a data dir and a working directory.
pub struct Env {
    pub root: tempfile::TempDir,
    pub data_dir: PathBuf,
    pub config: PathBuf,
    pub cwd: PathBuf,
    pub callback_port: u16,
}

/// A local port nothing listens on.
fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Env {
    pub fn new() -> Env {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let cwd = root.path().join("work");
        fs::create_dir(&cwd).unwrap();
        for dir in cwd.ancestors() {
            assert!(
                !dir.join(".env").exists(),
                "precondition: no .env above the test directory ({})",
                dir.display()
            );
        }
        let callback_port = closed_port();
        let config = root.path().join("archive.toml");
        fs::write(
            &config,
            format!(
                r#"data_dir = "{}"
oauth_client_id = "{CLIENT}"
access_region = "cn"
http_url = "https://openapi.longbridge.cn"
quote_ws_url = "wss://openapi-quote.longbridge.cn/v2"
callback_port = {callback_port}
research = ["AAPL.US"]
indexes = [".SPX.US"]
rps = 2
attempt_cap = 5
attempt_timeout_minutes = 25
carry_deadline_minutes = 60
window_start = "17:30"
notify_command = ["/bin/true"]
"#,
                data_dir.display()
            ),
        )
        .unwrap();
        Env {
            root,
            data_dir,
            config,
            cwd,
            callback_port,
        }
    }

    /// The binary with the controlled environment, in `cwd`.
    pub fn command(&self, args: &[&str]) -> Command {
        let proxy = format!("http://127.0.0.1:{}", closed_port());
        let mut command = Command::new(env!("CARGO_BIN_EXE_research-archive"));
        command
            .env_clear()
            .envs([
                ("HTTP_PROXY", &proxy),
                ("HTTPS_PROXY", &proxy),
                ("ALL_PROXY", &proxy),
            ])
            .current_dir(&self.cwd)
            .args(args)
            .arg("--config")
            .arg(&self.config);
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    pub fn token_path(&self) -> PathBuf {
        self.data_dir.join("oauth").join(format!("{CLIENT}.json"))
    }

    /// An SDK-written token file under the root, as
    /// `serde_json::to_string_pretty` of `OAuthToken`.
    pub fn sdk_token(&self, name: &str, client_id: &str, refresh: Option<&str>) -> PathBuf {
        let path = self.root.path().join(name);
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "client_id": client_id,
            "access_token": ACCESS,
            "refresh_token": refresh,
            "expires_at": 4_102_444_800u64,
        }))
        .unwrap();
        fs::write(&path, json).unwrap();
        path
    }
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

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
fn import_and_login_are_busy_while_the_lock_is_held() {
    let env = Env::new();
    let first = env.sdk_token("first", CLIENT, Some(REFRESH));
    let out = env.run(&["auth", "import", "--from", first.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stored = fs::read(env.token_path()).unwrap();

    let held = DataDirLock::try_acquire(&env.data_dir).unwrap();
    let second = env.sdk_token("second", CLIENT, None);
    for args in [
        &["auth", "import", "--from", second.to_str().unwrap()][..],
        &["auth", "login"][..],
    ] {
        let out = env.run(args);
        assert_eq!(
            out.status.code(),
            Some(0),
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
