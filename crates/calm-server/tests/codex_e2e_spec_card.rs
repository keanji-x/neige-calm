//! Issue #236 — end-to-end verification with a **real codex binary**.
//!
//! This test is feature-gated behind `codex-e2e` because CI doesn't
//! ship a `codex` binary. Run locally with:
//!
//! ```sh
//! cargo test --features codex-e2e --test codex_e2e_spec_card -- --nocapture
//! ```
//!
//! ## What it proves
//!
//! After `POST /api/waves`:
//!   1. The spec card's codex daemon is running.
//!   2. The codex process inherits `NEIGE_MCP_SOCKET` and
//!      `NEIGE_MCP_TOKEN` in its `/proc/<pid>/environ` — hard
//!      assertion (no soft-skip).
//!   3. The spec card's `$CODEX_HOME/config.toml` carries a
//!      `[mcp_servers.calm.env]` table containing both
//!      `NEIGE_MCP_SOCKET` and `NEIGE_MCP_TOKEN` with non-empty
//!      string values (#236 followup — codex CLI 0.132 doesn't
//!      forward the daemon env to MCP server subprocesses, so the
//!      env must live in the config.toml itself).
//!
//! Pre-fix (#236), the route returned 201 before the daemon was
//! spawned; if a WS attach raced the background `tokio::spawn`, the
//! respawn used the baked terminal-row env (no MCP vars). The
//! post-fix path is synchronous and the MCP env always lands.
//!
//! ## #236 followup — real `mcp_server` wired into the test fixture
//!
//! The initial cut of this test built `AppState::from_parts` (which
//! always sets `mcp_server: None`) and soft-skipped the env-presence
//! assertions when `state.mcp_server.is_none()`. That left a regression
//! window: the very bug the test was supposed to catch (codex env
//! missing the MCP vars after #236's sync-spawn change) couldn't be
//! caught here because the augmentation branch in
//! `routes::waves::create_wave` (lines 315-326) is gated on
//! `s.mcp_server.is_some()`. We now boot a real `McpServer` against a
//! tempdir-scoped UDS, assign it onto the `AppState` after
//! `from_parts`, and hard-assert both env vars are present. (Field is
//! `pub`; this is the documented test-fixture mutation seam — see the
//! `mcp_server` doc on `AppState`.)
//!
//! ## Self-skip
//!
//! If `NEIGE_CODEX_BIN` is unset or the resolved path is not
//! executable, the test `eprintln!`s an explicit skip marker and
//! returns success. We don't panic — the feature is opt-in but
//! must self-skip if the local environment is missing the binary.

#![cfg(all(unix, feature = "codex-e2e"))]

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
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const DEFAULT_CODEX_BIN: &str = "~/.nvm/versions/node/v24.4.1/bin/codex";

fn resolve_codex_bin() -> Option<PathBuf> {
    let raw = std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.to_string());
    // Best-effort tilde expansion (skip the `shellexpand` dep — we
    // only handle `~/...` since that's the documented shape).
    let expanded = if let Some(stripped) = raw.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        PathBuf::from(home).join(stripped)
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_file() {
        return None;
    }
    // Smoke-check the executable bit — symlinks to non-executables
    // would otherwise pass `is_file`.
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&expanded).ok()?;
    if meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    Some(expanded)
}
/// Issue #236 followup — locate the `neige-mcp-stdio-shim` binary the
/// codex daemon will spawn for the spec card. Same sibling-of-test-bin
/// resolver as the daemon helper above; depends on `cargo test
/// --workspace` (or an explicit `-p neige-mcp-stdio-shim --bin
/// neige-mcp-stdio-shim`) having built it.
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

/// Walk `/proc` looking for processes that are the codex binary the
/// test resolved. Matches by either:
///   * `/proc/<pid>/exe` resolving to `codex_bin` directly (Rust /
///     native shape), or
///   * `/proc/<pid>/cmdline` containing the resolved path as any
///     argv entry (node-script shape: `~/.nvm/.../codex` is a
///     symlink to `codex.js`, which runs under `node`).
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

/// Read `/proc/<pid>/environ` and return it as a list of (name, value)
/// pairs. Returns `None` if the file is unreadable (e.g., the process
/// exited between the `find` and the read).
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
async fn spec_card_codex_daemon_env_contains_mcp_vars() {
    let Some(codex_bin) = resolve_codex_bin() else {
        let raw =
            std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.to_string());
        eprintln!(
            "[codex-e2e] codex not found at {raw}; skipping (set NEIGE_CODEX_BIN to override)",
        );
        return;
    };
    eprintln!("[codex-e2e] using codex binary at {codex_bin:?}");

    // Note: `seed_and_spawn_spec_daemon` hard-codes `program = "codex"`,
    // so the daemon child runs `/bin/sh -c codex`. We need the resolved
    // `codex` to be on PATH for the shell to find it. Prepend the
    // codex bin's parent dir to PATH for this process; the daemon
    // inherits the parent process env when no override is set.
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
    let cove = repo
        .cove_create(NewCove {
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

    // Issue #236 followup — boot a real `McpServer` against a
    // tempdir-scoped UDS so `routes::waves::create_wave`'s env-
    // augmentation branch (lines 315-326) folds `NEIGE_MCP_SOCKET` +
    // `NEIGE_MCP_TOKEN` into the codex daemon's spawn env. With
    // `mcp_server = None` (the default `from_parts` shape), the
    // augmentation is gated out and the codex process inherits no MCP
    // vars — which is exactly the failure mode this test must guard
    // against. We mutate `state.mcp_server` after `from_parts` because
    // the field is `pub` and the doc on `AppState::mcp_server`
    // explicitly calls out test-fixture mutation as the documented seam.
    let mcp_socket_path = tmp.path().join("mcp").join("kernel.sock");
    let wave_cove_cache = WaveCoveCache::new();
    let mcp_server = McpServer::spawn(
        repo.clone(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        mcp_socket_path.clone(),
        locate_shim_bin(),
        build_default_registry(),
        None,
        std::env::temp_dir().join("neige-test-gate-logs"),
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
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
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

    // 1. POST /api/waves — synchronous spawn (#236). 201 means the
    //    daemon socket is up; codex is on its way up or already
    //    running inside the daemon.
    let (status, body) = post(
        app.clone(),
        "/api/waves",
        json!({"cove_id": cove.id, "title": "codex-e2e wave", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "wave create returned non-201; body={body}",
    );

    // 1a. #236 followup — find the spec card the route just minted
    //     and assert its per-card `$CODEX_HOME/config.toml` carries
    //     the `[mcp_servers.calm.env]` block with both MCP vars baked
    //     into it. This is the minimum-cost surface for the codex →
    //     shim env boundary: the pre-followup code relied on codex
    //     forwarding the daemon's env to MCP subprocesses, which
    //     codex CLI 0.132 doesn't do — the shim exited with
    //     `missing NEIGE_MCP_SOCKET` and the spec agent had no way
    //     to reach the kernel.
    let wave_id = body
        .get("id")
        .and_then(Value::as_str)
        .expect("wave id in response");
    let spec_cards_body = {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/waves/{wave_id}/cards"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null)
    };
    // The spec card is the only kernel-owned (`deletable = false`) card
    // minted on a fresh wave (`model::Card::deletable` flips to `false`
    // for spec cards per #229 PR A — see `create_wave` in
    // `routes::waves`). `Card` doesn't expose the `role` field on the
    // wire (that lives in `card_roles`); deletable is the next-best
    // stable discriminator here.
    let spec_card_id = spec_cards_body
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
            panic!("spec card present on freshly created wave; cards body: {spec_cards_body}")
        });
    let codex_home = state.codex.codex_homes_dir.join(&spec_card_id);
    let cfg_path = codex_home.join("config.toml");
    let cfg_text =
        std::fs::read_to_string(&cfg_path).unwrap_or_else(|e| panic!("read {cfg_path:?}: {e}"));
    eprintln!(
        "[codex-e2e] spec card config.toml ({} bytes):\n{cfg_text}",
        cfg_text.len(),
    );
    assert!(
        cfg_text.contains("[mcp_servers.calm]"),
        "spec card config.toml missing `[mcp_servers.calm]` block; got:\n{cfg_text}",
    );
    assert!(
        cfg_text.contains("[mcp_servers.calm.env]"),
        "spec card config.toml missing `[mcp_servers.calm.env]` block — codex won't pass MCP vars to the shim subprocess (#236 followup); got:\n{cfg_text}",
    );
    // Hard-assert both env keys are present with non-empty string
    // values. We don't compare to the exact token (it's minted
    // per-card and not surfaced through any read API), but we can
    // verify the line shape and that the value isn't an empty
    // string.
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

    // 2. Wait for a *new* codex process to appear. The shell -c
    //    codex hop can take a moment; we allow up to 10 s.
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

    // Issue #236 followup — hard assertions. With the real `McpServer`
    // wired into the test fixture (see the boot block above), the
    // create-wave handler's env-augmentation branch must have folded
    // both vars into the codex daemon's env. A soft-skip here would
    // re-open the exact regression window that landed the followup
    // fixes (shim token injection + docker mount): the bug is
    // "codex starts but the MCP handshake never authenticates";
    // checking env presence is the cheap surface, the handshake
    // attempt below is the deep one.
    assert!(
        state.mcp_server.is_some(),
        "[codex-e2e] test must wire a real mcp_server (see #236 followup); got None"
    );
    assert!(
        !socket.is_empty(),
        "[codex-e2e] codex env missing NEIGE_MCP_SOCKET — wave-create env augmentation \
         didn't fire (routes/waves.rs lines 315-326) or the codex process exec'd before the \
         env reached it",
    );
    assert!(
        !token.is_empty(),
        "[codex-e2e] codex env missing NEIGE_MCP_TOKEN — wave-create env augmentation \
         didn't mint a per-card token or didn't fold it into the spawn env",
    );

    // Bonus: drive a real MCP `initialize` through the shim using the
    // same token + socket the codex daemon would. Proves end-to-end
    // that the post-#236-followup shim:
    //   * accepts the env vars,
    //   * opens the UDS,
    //   * injects the token into `params._meta["dev.neige/auth"]`,
    //   * the kernel's `handle_initialize` accepts that token,
    //   * a success-shaped response makes it back through stdout.
    //
    // We re-use the token+socket the codex daemon received (read
    // from `/proc/<pid>/environ` above) — that's the same per-card
    // identity binding the codex daemon would present, so a success
    // here exactly matches what codex's MCP client sees.
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

    // Cleanup: best-effort kill the codex child so we don't leak it
    // between test runs. The daemon's wait loop will also reap it,
    // but tempdir drops first. Shell out to /bin/kill to avoid
    // pulling `libc` / `nix` into dev-deps for this one signal.
    let _ = std::process::Command::new("/bin/kill")
        .arg("-TERM")
        .arg(new_pid.to_string())
        .status();
}
