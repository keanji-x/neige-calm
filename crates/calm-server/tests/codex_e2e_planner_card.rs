//! End-to-end verification with a real codex binary; feature-gated behind `codex-e2e` and self-skips when `NEIGE_CODEX_BIN` is unset.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
// Env `NEIGE_CODEX_BIN` only, `None` ⇒ self-skip; tests must never fall back to a PATH/home codex binary.
use support::codex_fixture::resolve_codex_bin;
use tempfile::TempDir;
use tower::ServiceExt;

/// Locate the `neige-mcp-stdio-shim` binary next to the test binary; requires `cargo test --workspace` to have built it.
fn locate_shim_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("neige-mcp-stdio-shim");
    assert!(
        p.exists(),
        "neige-mcp-stdio-shim not found at {p:?}; run \
         `cargo build -p neige-mcp-stdio-shim --bin neige-mcp-stdio-shim` first, or \
         use `cargo test --workspace` which builds workspace bins",
    );
    p
}

/// Walk `/proc` for processes running `codex_bin`, by `exe` link or by any `cmdline` argv entry (node-script shape).
fn find_codex_pids(codex_bin: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    // For the node-script shape we also follow the symlink to the
    // canonical script path and match argv entries against either.
    let codex_canonical = std::fs::canonicalize(codex_bin).ok();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let exe_link = entry.path().join("exe");
        if let Ok(target) = std::fs::read_link(&exe_link)
            && (target == codex_bin || Some(&target) == codex_canonical.as_ref())
        {
            out.push(pid);
            continue;
        }
        // Fall back to cmdline matching (NUL-separated argv).
        if let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) {
            for arg in cmdline.split(|&b| b == 0) {
                if arg.is_empty() {
                    continue;
                }
                let Ok(s) = std::str::from_utf8(arg) else {
                    continue;
                };
                let arg_path = Path::new(s);
                if arg_path == codex_bin
                    || Some(arg_path.to_path_buf()) == codex_canonical
                    || std::fs::canonicalize(arg_path).ok() == codex_canonical
                {
                    out.push(pid);
                    break;
                }
            }
        }
    }
    out
}

/// Read `/proc/<pid>/environ` as (name, value) pairs; `None` if the process exited in between.
fn read_proc_environ(pid: u32) -> Option<Vec<(String, String)>> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let mut out = Vec::new();
    for chunk in bytes.split(|&b| b == 0) {
        if chunk.is_empty() {
            continue;
        }
        let s = std::str::from_utf8(chunk).ok()?;
        if let Some((k, v)) = s.split_once('=') {
            out.push((k.to_string(), v.to_string()));
        }
    }
    Some(out)
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn planner_card_codex_daemon_env_contains_mcp_vars() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };
    eprintln!("[codex-e2e] using codex binary at {codex_bin:?}");

    // `seed_and_spawn_planner_daemon` hard-codes `program = "codex"`, so the resolved binary's dir must be on PATH for `/bin/sh -c codex`.
    if let Some(parent) = codex_bin.parent() {
        let existing = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", format!("{}:{existing}", parent.display()));
        }
    }

    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "codex-e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();

    // A real `McpServer` on a tempdir UDS: with `mcp_server = None` the env-augmentation branch is gated out and no MCP vars reach codex.
    let mcp_socket_path = tmp.path().join("mcp").join("kernel.sock");
    let track_area_cache = TrackAreaCache::new();
    let mcp_server = McpServer::spawn(
        repo.clone(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
        mcp_socket_path.clone(),
        locate_shim_bin(),
        build_default_registry(),
        None,
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::sync::Arc::new(tokio::sync::OnceCell::new()),
        std::env::temp_dir().join("neige-test-gate-logs"),
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .expect("boot test mcp server");
    eprintln!(
        "[codex-e2e] mcp server listening at {} (shim: {})",
        mcp_socket_path.display(),
        mcp_server.shim_config.shim_bin.display(),
    );

    let mut state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-codex-e2e"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );
    state.mcp_server = Some(mcp_server.clone());

    let state_for_router = state.clone();
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state_for_router);

    let baseline_pids: std::collections::HashSet<u32> =
        find_codex_pids(&codex_bin).into_iter().collect();
    eprintln!(
        "[codex-e2e] baseline codex pids (pre-create): {} entries",
        baseline_pids.len(),
    );

    // 1. POST /api/tracks — 201 means the daemon socket is up.
    let (status, body) = post(
        app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": area.id, "title": "codex-e2e track", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "track create returned non-201; body={body}",
    );

    // 1a. The planner card's `$CODEX_HOME/config.toml` must carry `[mcp_servers.calm.env]`: codex CLI does not forward the daemon env to MCP subprocesses.
    let track_id = body
        .get("id")
        .and_then(Value::as_str)
        .expect("track id in response");
    let planner_cards_body = {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/tracks/{track_id}/cards"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null)
    };
    // The planner card is the only kernel-owned (`deletable = false`) card on a fresh track; `role` is not on the wire.
    let planner_card_id = planner_cards_body
        .as_array()
        .and_then(|cards| {
            cards.iter().find_map(|c| {
                if c.get("deletable").and_then(Value::as_bool) == Some(false) {
                    c.get("id").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "planner card present on freshly created track; cards body: {planner_cards_body}"
            )
        });
    let codex_home = state.codex.codex_homes_dir.join(&planner_card_id);
    let cfg_path = codex_home.join("config.toml");
    let cfg_text =
        std::fs::read_to_string(&cfg_path).unwrap_or_else(|e| panic!("read {cfg_path:?}: {e}"));
    eprintln!(
        "[codex-e2e] planner card config.toml ({} bytes):\n{cfg_text}",
        cfg_text.len(),
    );
    assert!(
        cfg_text.contains("[mcp_servers.calm]"),
        "planner card config.toml missing `[mcp_servers.calm]` block; got:\n{cfg_text}",
    );
    assert!(
        cfg_text.contains("[mcp_servers.calm.env]"),
        "planner card config.toml missing `[mcp_servers.calm.env]` block — codex won't pass MCP vars to the shim subprocess (#236 followup); got:\n{cfg_text}",
    );
    // The token is minted per-card and not surfaced by any read API, so only the line shape and non-emptiness are checked.
    let env_socket_line = cfg_text
        .lines()
        .find(|l| l.trim_start().starts_with("NEIGE_MCP_SOCKET ="))
        .expect("config.toml must declare NEIGE_MCP_SOCKET in env block");
    let env_token_line = cfg_text
        .lines()
        .find(|l| l.trim_start().starts_with("NEIGE_MCP_TOKEN ="))
        .expect("config.toml must declare NEIGE_MCP_TOKEN in env block");
    // Pull the value out of `KEY = "value"` and assert non-empty.
    let extract = |line: &str| -> String {
        let value = line.split_once('=').map(|x| x.1.trim()).unwrap_or("");
        value.trim_matches('"').to_string()
    };
    let socket_in_toml = extract(env_socket_line);
    let token_in_toml = extract(env_token_line);
    assert!(
        !socket_in_toml.is_empty(),
        "NEIGE_MCP_SOCKET value in config.toml is empty: {env_socket_line:?}",
    );
    assert!(
        !token_in_toml.is_empty(),
        "NEIGE_MCP_TOKEN value in config.toml is empty: {env_token_line:?}",
    );
    eprintln!(
        "[codex-e2e] config.toml env block OK — NEIGE_MCP_SOCKET=\"{}\" (len {}), NEIGE_MCP_TOKEN=<len {}>",
        socket_in_toml,
        socket_in_toml.len(),
        token_in_toml.len(),
    );

    // 2. Wait for a *new* codex process; the `sh -c codex` hop can take a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    let new_pid = loop {
        let now = find_codex_pids(&codex_bin);
        let candidate = now.into_iter().find(|p| !baseline_pids.contains(p));
        if let Some(pid) = candidate {
            break pid;
        }
        if Instant::now() > deadline {
            panic!(
                "[codex-e2e] no new codex pid appeared within 10 s; \
                 baseline={baseline_pids:?}; codex_bin={codex_bin:?}",
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    eprintln!("[codex-e2e] new codex pid: {new_pid}");

    // 3. Grep its environ.
    let environ = read_proc_environ(new_pid)
        .unwrap_or_else(|| panic!("[codex-e2e] could not read /proc/{new_pid}/environ"));
    let env_keys: std::collections::HashMap<&str, &str> = environ
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let socket = env_keys.get("NEIGE_MCP_SOCKET").copied().unwrap_or("");
    let token = env_keys.get("NEIGE_MCP_TOKEN").copied().unwrap_or("");
    eprintln!(
        "[codex-e2e] NEIGE_MCP_SOCKET present={} (len={}); NEIGE_MCP_TOKEN present={} (len={})",
        !socket.is_empty(),
        socket.len(),
        !token.is_empty(),
        token.len(),
    );

    assert!(
        state.mcp_server.is_some(),
        "[codex-e2e] test must wire a real mcp_server (see #236 followup); got None"
    );
    assert!(
        !socket.is_empty(),
        "[codex-e2e] codex env missing NEIGE_MCP_SOCKET — track-create env augmentation \
         didn't fire (routes/tracks.rs lines 315-326) or the codex process exec'd before the \
         env reached it",
    );
    assert!(
        !token.is_empty(),
        "[codex-e2e] codex env missing NEIGE_MCP_TOKEN — track-create env augmentation \
         didn't mint a per-card token or didn't fold it into the spawn env",
    );

    // Drive a real MCP `initialize` through the shim with the token + socket the codex daemon received, the same per-card identity codex's MCP client would present.
    let shim_bin = locate_shim_bin();
    eprintln!("[codex-e2e] driving MCP handshake through shim at {shim_bin:?}");
    let mut shim_child = tokio::process::Command::new(&shim_bin)
        .env("NEIGE_MCP_SOCKET", socket)
        .env("NEIGE_MCP_TOKEN", token)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn shim");
    let mut shim_stdin = shim_child.stdin.take().expect("shim stdin piped");
    let shim_stdout = shim_child.stdout.take().expect("shim stdout piped");

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let init_frame = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "codex-e2e-test", "version": "0"}
        }
    });
    shim_stdin
        .write_all(format!("{init_frame}\n").as_bytes())
        .await
        .expect("write initialize");
    shim_stdin.flush().await.expect("flush initialize");

    let mut reader = BufReader::new(shim_stdout);
    let mut resp_line = String::new();
    let read_fut = reader.read_line(&mut resp_line);
    let resp_n = tokio::time::timeout(Duration::from_secs(5), read_fut)
        .await
        .expect("kernel initialize response within budget")
        .expect("read response line");
    assert!(resp_n > 0, "[codex-e2e] shim hung up without responding");
    let resp: Value = serde_json::from_str(resp_line.trim_end())
        .unwrap_or_else(|e| panic!("[codex-e2e] non-JSON response {resp_line:?}: {e}"));
    // A success-shaped response carries `result.protocolVersion`; an
    // auth failure would carry `error.code = -32602` or `-32401`.
    assert!(
        resp.get("result").is_some(),
        "[codex-e2e] handshake failed; response: {resp}"
    );
    assert!(
        resp["result"]["protocolVersion"].is_string(),
        "[codex-e2e] result missing protocolVersion: {resp}"
    );
    eprintln!(
        "[codex-e2e] handshake succeeded; protocolVersion={}",
        resp["result"]["protocolVersion"]
    );

    // Wind down the shim cleanly.
    drop(shim_stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), shim_child.wait()).await;

    // Best-effort kill of the codex child via /bin/kill, avoiding a `libc`/`nix` dev-dep for one signal.
    let _ = std::process::Command::new("/bin/kill")
        .arg("-TERM")
        .arg(new_pid.to_string())
        .status();
}
