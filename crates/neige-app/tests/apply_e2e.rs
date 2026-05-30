use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};

struct Harness {
    root: PathBuf,
    data_dir: PathBuf,
    release_root: PathBuf,
    admin: SocketAddr,
    calm: SocketAddr,
    token: String,
    app: Child,
    _proc_sock_thread: thread::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.app.kill();
        let _ = self.app.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "binds sockets and spawns neige-app; blocked by the Codex sandbox"]
fn apply_preserving_swap_keeps_pty() -> anyhow::Result<()> {
    let mut h = Harness::start("preserving", "0.1.0")?;
    let package = h.package(
        "rel-2",
        [("calmServer", "0.2.0", "restartViaAdminApi")],
        true,
    )?;

    let resp = h.post_json(
        "/upgrade/apply",
        json!({"source": {"url": package.display().to_string()}}),
    )?;

    assert_eq!(resp.status, 200, "body: {}", resp.body);
    assert_eq!(resp.json["result"], "committed");
    assert_eq!(resp.json["verdict"]["kind"], "preserving");
    assert_eq!(resp.json["unitsChanged"], json!(["calmServer"]));
    assert_eq!(h.version()?, "0.2.0");
    assert!(
        h.data_dir
            .join("state")
            .join("release-history.jsonl")
            .is_file()
    );
    assert_eq!(
        read_json(&h.data_dir.join("state").join("installed.json"))?["releaseId"],
        "rel-2"
    );
    Ok(())
}

#[test]
#[ignore = "binds sockets and spawns neige-app; blocked by the Codex sandbox"]
fn apply_preserving_supervisor_change_defers() -> anyhow::Result<()> {
    let mut h = Harness::start("defer", "0.1.0")?;
    let package = h.package(
        "rel-supervisor",
        [("calmProcSupervisor", "0.2.0", "deferUntilFullReboot")],
        true,
    )?;
    let before = fs::read_link(h.release_root.join("current-server"))?;

    let resp = h.post_json(
        "/upgrade/apply",
        json!({"source": {"url": package.display().to_string()}}),
    )?;

    assert_eq!(resp.status, 200, "body: {}", resp.body);
    assert_eq!(resp.json["result"], "committed");
    assert_eq!(resp.json["verdict"]["kind"], "preserving");
    assert_eq!(resp.json["deferred"], json!(["calmProcSupervisor"]));
    let after = fs::read_link(h.release_root.join("current-server"))?;
    assert_ne!(before, after, "server release symlink should move");
    assert_eq!(h.version()?, "0.1.0", "calm-server should not restart");
    Ok(())
}

#[test]
#[ignore = "binds sockets and spawns neige-app; blocked by the Codex sandbox"]
fn apply_preserving_healthcheck_fail_rolls_back() -> anyhow::Result<()> {
    let mut h = Harness::start("rollback", "0.1.0")?;
    let db = h.data_dir.join("calm.db");
    fs::write(&db, "old-db")?;
    let package = h.package(
        "rel-bad",
        [("calmServer", "0.2.0", "restartViaAdminApi")],
        false,
    )?;
    let before = fs::read_link(h.release_root.join("current-server"))?;

    let resp = h.post_json(
        "/upgrade/apply",
        json!({"source": {"url": package.display().to_string()}}),
    )?;

    assert_eq!(resp.status, 502, "body: {}", resp.body);
    assert_eq!(resp.json["result"], "rolledBack");
    assert_eq!(
        fs::read_link(h.release_root.join("current-server"))?,
        before
    );
    assert_eq!(fs::read_to_string(&db)?, "old-db");
    assert_eq!(h.version()?, "0.1.0");
    Ok(())
}

#[test]
#[ignore = "binds sockets and spawns neige-app; blocked by the Codex sandbox"]
fn apply_breaking_without_opt_in_rejects() -> anyhow::Result<()> {
    let mut h = Harness::start("breaking-reject", "0.1.0")?;
    let package = h.breaking_package("rel-breaking")?;

    let resp = h.post_json(
        "/upgrade/apply",
        json!({"source": {"url": package.display().to_string()}}),
    )?;

    assert_eq!(resp.status, 400, "body: {}", resp.body);
    assert_eq!(resp.json["result"], "rejected");
    assert_eq!(resp.json["verdict"]["kind"], "breaking");
    Ok(())
}

#[test]
#[ignore = "exec-self replaces the test child process image; run manually outside cargo test"]
fn apply_breaking_opt_in_then_exec_self() -> anyhow::Result<()> {
    let mut h = Harness::start("breaking-exec", "0.1.0")?;
    let package = h.breaking_package("rel-breaking")?;
    let resp = h.post_json(
        "/upgrade/apply",
        json!({"source": {"url": package.display().to_string()}, "allowBreaking": true}),
    )?;
    assert_eq!(resp.status, 202, "body: {}", resp.body);
    assert_eq!(resp.json["result"], "committed");
    Ok(())
}

impl Harness {
    fn start(name: &str, initial_version: &str) -> anyhow::Result<Self> {
        let root =
            std::env::temp_dir().join(format!("neige-app-apply-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        let data_dir = root.join("data");
        let release_root = root.join("releases");
        fs::create_dir_all(&data_dir)?;
        fs::create_dir_all(&release_root)?;
        let admin = free_addr()?;
        let calm = free_addr()?;
        let token = "test-token".to_string();
        let db = data_dir.join("calm.db");
        fs::write(&db, "initial-db")?;

        let old = root.join("old-release");
        fs::create_dir_all(old.join("bin"))?;
        write_calm_server(&old.join("bin").join("calm-server"), initial_version, true)?;
        write_executable(
            &old.join("bin").join("calm-proc-supervisor"),
            "#!/bin/sh\nsleep 300\n",
        )?;
        write_executable(&old.join("bin").join("neige-app"), "#!/bin/sh\nsleep 300\n")?;
        make_symlink(&old, &release_root.join("current-server"))?;
        make_symlink(&old, &release_root.join("current-web"))?;
        write_installed(
            &data_dir,
            "rel-1",
            0,
            initial_version,
            [("calmServer", initial_version)],
        )?;

        let config = root.join("config.toml");
        fs::write(
            &config,
            format!(
                r#"[admin]
listen = "{admin}"
token_file = ""

[release]
root = "{release_root}"
current_server = "{current_server}"
current_web = "{current_web}"
previous_server = "{previous_server}"
previous_web = "{previous_web}"
backups = "{backups}"

[child]
bin = "{child_bin}"
proc_supervisor_bin = "{proc_bin}"
web_dist = "{web_dist}"
calm_listen = "{calm}"
db_url = "sqlite://{db}"
data_dir = "{data_dir}"
mcp_stdio_shim_bin = ""
auth_dev_autologin = true
extra_args = []

[timing]
stop_grace_ms = 1000
restart_delay_ms = 100

[systemd]
unit_path = "{unit}"
unit_name = "neige-app-test"
bin = "{neige_bin}"

[source]
url = ""
branch = "main"
checkout_dir = "{checkout}"
build_args = ["true"]
"#,
                admin = admin,
                release_root = release_root.display(),
                current_server = release_root.join("current-server").display(),
                current_web = release_root.join("current-web").display(),
                previous_server = release_root.join("previous-server").display(),
                previous_web = release_root.join("previous-web").display(),
                backups = release_root.join("backups").display(),
                child_bin = release_root
                    .join("current-server")
                    .join("bin")
                    .join("calm-server")
                    .display(),
                proc_bin = release_root
                    .join("current-server")
                    .join("bin")
                    .join("calm-proc-supervisor")
                    .display(),
                web_dist = release_root
                    .join("current-web")
                    .join("web")
                    .join("dist")
                    .display(),
                calm = calm,
                data_dir = data_dir.display(),
                db = db.display(),
                unit = root.join("unit.service").display(),
                neige_bin = locate_neige_app().display(),
                checkout = root.join("checkout").display(),
            ),
        )?;

        let proc_sock = data_dir.join("proc-supervisor.sock");
        let proc_sock_thread = spawn_proc_sock(&proc_sock)?;
        let mut app = Command::new(locate_neige_app())
            .args(["system", "serve", "--config"])
            .arg(&config)
            .args(["--admin-token", &token])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        if let Err(err) = wait_http(admin, "/health", None) {
            let _ = app.kill();
            return Err(err);
        }

        Ok(Self {
            root,
            data_dir,
            release_root,
            admin,
            calm,
            token,
            app,
            _proc_sock_thread: proc_sock_thread,
        })
    }

    fn version(&self) -> anyhow::Result<String> {
        let resp = wait_http(self.calm, "/api/version", None)?;
        Ok(resp.json["kernelVersion"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing kernelVersion"))?
            .to_string())
    }

    fn post_json(&mut self, path: &str, body: serde_json::Value) -> anyhow::Result<HttpResp> {
        http_json("POST", self.admin, path, Some(&self.token), Some(body))
    }

    fn package<const N: usize>(
        &self,
        release_id: &str,
        changed: [(&str, &str, &str); N],
        healthy: bool,
    ) -> anyhow::Result<PathBuf> {
        let dir = self.root.join(release_id);
        fs::create_dir_all(dir.join("bin"))?;
        let calm_version = changed
            .iter()
            .find(|(unit, _, _)| *unit == "calmServer")
            .map(|(_, version, _)| *version)
            .unwrap_or("0.1.0");
        write_calm_server(&dir.join("bin").join("calm-server"), calm_version, healthy)?;
        write_executable(
            &dir.join("bin").join("calm-proc-supervisor"),
            "#!/bin/sh\nsleep 300\n",
        )?;
        write_executable(&dir.join("bin").join("neige-app"), "#!/bin/sh\nsleep 300\n")?;
        write_manifest(
            &dir,
            release_id,
            0,
            changed,
            if healthy { "none" } else { "forwardOnly" },
        )?;
        Ok(dir)
    }

    fn breaking_package(&self, release_id: &str) -> anyhow::Result<PathBuf> {
        let dir = self.package(
            release_id,
            [("calmServer", "0.2.0", "restartViaAdminApi")],
            true,
        )?;
        write_manifest(
            &dir,
            release_id,
            1,
            [("calmServer", "0.2.0", "restartViaAdminApi")],
            "none",
        )?;
        Ok(dir)
    }
}

struct HttpResp {
    status: u16,
    body: String,
    json: serde_json::Value,
}

fn http_json(
    method: &str,
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> anyhow::Result<HttpResp> {
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    let mut stream = TcpStream::connect(addr)?;
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    read_http(stream)
}

fn wait_http(addr: SocketAddr, path: &str, token: Option<&str>) -> anyhow::Result<HttpResp> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    while Instant::now() < deadline {
        match http_json("GET", addr, path, token, None) {
            Ok(resp) if resp.status == 200 => return Ok(resp),
            Ok(resp) => last = Some(anyhow::anyhow!("HTTP {}", resp.status)),
            Err(err) => last = Some(err),
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("timed out")))
}

fn read_http(mut stream: TcpStream) -> anyhow::Result<HttpResp> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response: {text}"))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("missing HTTP status"))?
        .parse()?;
    let json = serde_json::from_str(body.trim()).unwrap_or(serde_json::Value::Null);
    Ok(HttpResp {
        status,
        body: body.to_string(),
        json,
    })
}

fn free_addr() -> anyhow::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?)
}

fn spawn_proc_sock(path: &Path) -> anyhow::Result<thread::JoinHandle<()>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let path = path.to_path_buf();
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    Ok(thread::spawn(move || {
        for stream in listener.incoming() {
            if stream.is_err() {
                break;
            }
        }
    }))
}

fn write_calm_server(path: &Path, version: &str, healthy: bool) -> anyhow::Result<()> {
    let behavior = if healthy {
        "httpd.serve_forever()"
    } else {
        "sys.exit(1)"
    };
    write_executable(
        path,
        &format!(
            r#"#!/usr/bin/env python3
import http.server, json, os, socketserver, sys
listen = os.environ.get("CALM_LISTEN", "127.0.0.1:4040")
host, port = listen.rsplit(":", 1)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass
    def do_GET(self):
        if self.path == "/api/version":
            body = json.dumps({{"kernelVersion":"{version}"}}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()
socketserver.TCPServer.allow_reuse_address = True
httpd = socketserver.TCPServer((host, int(port)), H)
{behavior}
"#,
            version = version,
            behavior = behavior
        ),
    )
}

fn write_executable(path: &Path, content: &str) -> anyhow::Result<()> {
    fs::write(path, content)?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn write_manifest<const N: usize>(
    dir: &Path,
    release_id: &str,
    product_major: u32,
    changed: [(&str, &str, &str); N],
    calm_db_policy: &str,
) -> anyhow::Result<()> {
    let mut units = BTreeMap::new();
    for (unit, version, restart_policy) in changed {
        units.insert(
            unit.to_string(),
            json!({
                "version": version,
                "binarySha256": "a".repeat(64),
                "restartPolicy": restart_policy,
                "dbMigrationPolicy": if unit == "calmServer" { calm_db_policy } else { "none" }
            }),
        );
    }
    let files = manifest_files(dir)?;
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 2,
            "releaseId": release_id,
            "productMajor": product_major,
            "compatibility": compatibility(),
            "units": units,
            "files": files,
        }))?,
    )?;
    Ok(())
}

fn manifest_files(dir: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut files = Vec::new();
    for name in ["calm-server", "calm-proc-supervisor", "neige-app"] {
        let rel = format!("bin/{name}");
        let path = dir.join(&rel);
        let bytes = fs::read(&path)?;
        let hash = Sha256::digest(&bytes);
        files.push(json!({
            "path": rel,
            "sha256": format!("{hash:x}"),
            "bytes": bytes.len(),
            "unit": if name == "calm-server" { "calmServer" } else { "bundle" }
        }));
    }
    Ok(files)
}

fn write_installed<const N: usize>(
    data_dir: &Path,
    release_id: &str,
    product_major: u32,
    calm_version: &str,
    units_in: [(&str, &str); N],
) -> anyhow::Result<()> {
    let mut units = serde_json::Map::new();
    for (unit, version) in units_in {
        units.insert(
            unit.to_string(),
            json!({"version": version, "binarySha256": "a".repeat(64)}),
        );
    }
    fs::create_dir_all(data_dir.join("state"))?;
    fs::write(
        data_dir.join("state").join("installed.json"),
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 1,
            "releaseId": release_id,
            "productMajor": product_major,
            "compatibility": compatibility(),
            "units": units,
            "installedAt": "2026-05-30T00:00:00Z",
            "calmVersionForDebug": calm_version
        }))?,
    )?;
    Ok(())
}

fn compatibility() -> serde_json::Value {
    json!({
        "terminalFrameVersion": 4,
        "terminalProtocolVersion": 4,
        "apiVersion": "1",
        "syncEventVersion": 1,
        "mcpProtocolVersion": "2024-11-05",
        "pluginMcpProtocolVersion": "2025-11-25",
        "webCompatVersion": 2,
        "minWebCompatVersion": 2,
        "supervisorControlVersion": 1
    })
}

fn read_json(path: &Path) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn make_symlink(target: &Path, link: &Path) -> anyhow::Result<()> {
    let _ = fs::remove_file(link);
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

fn locate_neige_app() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_neige-app")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .expect("current exe")
                .parent()
                .and_then(|p| p.parent())
                .expect("target profile")
                .join("neige-app")
        })
}
