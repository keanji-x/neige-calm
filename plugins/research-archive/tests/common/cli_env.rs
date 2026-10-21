//! The `research-archive` binary as a subprocess, in a private directory
//! with a config. Shared by the suite's CLI tests and the `net` binary's
//! I11 test. Every run gets an empty environment except proxies that point
//! at a closed local port, so even a mistaken network call could not leave
//! the machine.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

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
benchmarks = []
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
