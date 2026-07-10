//! #840 slice (e2) — SIGABRT inside the "gh merge landed, fence not yet
//! committed" window must not double-merge (danger-point-2).
//!
//! The in-process analog (`git_forge_merge_crash_recovers_once_via_probe`,
//! forge_workflow_e2e.rs) already proves the recovery *code path*; this test's
//! sole value-add is **real process death + real binary reboot**:
//!
//!   boot#1 the shipped `calm-server` binary (fixtures build, so the
//!   `test_seams::crash_point` seam exists) with
//!   `CALM_TEST_CRASH_AT=forge-pre-fence-commit:forge.pr.merged` → drive a
//!   real PR through the kernel MCP socket + git-forge plugin + gh shim →
//!   `gh.pr.merge` runs the irreversible action, then the kernel aborts
//!   **immediately before `tx.commit()`** of the completion fence
//!   (`complete_forge_op_succeeded`) → boot#2 the same binary against the
//!   same durable tempdir with the seam unarmed → recovery replays the
//!   durable result file (never re-runs gh).
//!
//! Invariant: gh-shim `pr_merge_count == 1` across abort+reboot, exactly one
//! `forge.pr.merged` event, op phase `succeeded`, and the fence tx
//! demonstrably rolled back at the crash (op still `parked`, zero merged
//! events, in the crash window).
//!
//! Anti-vacuity: step "wait for boot#1 to die" asserts the exit was SIGABRT —
//! if the seam were compiled out or never reached, boot#1 keeps running and
//! the test fails on that assert; no silent pass is possible.
//!
//! Safety: same tier as e1 — spawns ONLY the calm-server binary + the
//! git-forge plugin stub + a hermetic `gh` shim inside a throwaway tempdir
//! (env-cleared allowlist, ephemeral non-4040 port, nonexistent codex/claude
//! binaries, Drop-guard SIGKILL). Self-skips if the sandbox denies a loopback
//! bind. No real codex, no real gh, CI-safe.

#![cfg(target_os = "linux")]

mod support;

use std::ffi::{OsStr, OsString};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, card_with_codex_create_tx, session_bind_attribution_tx,
    session_mcp_token_set_tx, session_projection_active_for_card_tx, session_start_runtime_tx,
};
use calm_server::model::{CardRole, NewCove, NewPlugin, NewWave, now_ms};
use calm_server::plugin_host::Manifest;
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use support::gh_shim::write_gh_shim;
use support::git_helpers::{
    clone_for_wave, configure_repo_identity, init_bare_origin, run_git, run_git_capture,
    stage_git_change,
};
use support::kernel_proc::{launch_kernel, wait_exit_with_timeout};
use support::mcp::{call_tool_via_socket, send_tool_call_without_reply};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};

const FORGE_BIN: &str = env!("CARGO_BIN_EXE_git-forge");
const PLUGIN_ID: &str = "dev.neige.git-forge";
const PR_CREATE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.create";
const PR_MERGE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.merge";
const CRASH_POINT: &str = "forge-pre-fence-commit:forge.pr.merged";

/// How long boot#1 gets to run the merge action and hit the abort seam, and
/// how long boot#2's recovery gets to reach `succeeded`. Generous: the real
/// binary path is plugin lower → wrapper spawn → gh shim → observer.
const CRASH_TIMEOUT: Duration = Duration::from_secs(30);
const ORACLE_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_abort_pre_fence_commit_then_reboot_merges_exactly_once() {
    // ---- prod-safety hard guards (never touch the real DB / port) ---------
    let tmp: TempDir = socket_safe_tempdir().expect("tempdir");
    let tmp_path: PathBuf = tmp.path().to_path_buf();
    let db_path = tmp_path.join("calm.db");
    let db_str = db_path.to_string_lossy().to_string();
    assert!(
        !db_str.contains("/.local/share/neige-calm"),
        "test DB must never be the prod DB: {db_str}"
    );
    assert!(
        tmp_path.starts_with(std::env::temp_dir())
            || tmp_path.to_string_lossy().starts_with("/tmp"),
        "test tmpdir must live under the system temp dir: {}",
        tmp_path.display()
    );
    let db_url = format!("sqlite://{db_str}?mode=rwc");

    // ---- world seeding (before boot#1) -------------------------------------
    let wave_cwd = tmp_path.join("wave-cwd");
    std::fs::create_dir_all(&wave_cwd).expect("create wave cwd");
    let origin_repo = tmp_path.join("origin.git");
    init_bare_origin(&origin_repo, &tmp_path.join("seed"));
    clone_for_wave(&origin_repo, &wave_cwd);

    let shim_dir = tmp_path.join("shim-bin");
    std::fs::create_dir_all(&shim_dir).expect("create gh shim dir");
    write_gh_shim(&shim_dir);

    install_git_forge_plugin_files(&tmp_path);

    // ONE reader pool for the whole test, opened BEFORE boot#1 (this also runs
    // the migrations). Never re-`open()` against a live kernel — the migration
    // check would race a live writer; all polls below go through this pool and
    // tolerate transient SQLITE_BUSY while a kernel is writing.
    let repo = Arc::new(SqlxRepo::open(&db_url).await.expect("open file db"));
    let seeded = seed_world(&repo, &wave_cwd).await;
    seed_plugin_row(&repo, &tmp_path).await;
    provision_worker_worktree(
        &wave_cwd,
        &seeded.wave_id,
        &seeded.card_id,
        &seeded.lease_abs,
    );

    let path_value = prepend_to_path(&shim_dir);
    let base_env: Vec<(&str, OsString)> = vec![
        // The wrapper/probe subprocess env re-reads the *kernel's* PATH
        // (`apply_forge_subprocess_env`), so prepending the shim dir here
        // guarantees `gh` can only ever resolve to the shim, on BOTH boots.
        ("PATH", path_value),
        ("NEIGE_TRUSTED_FORGE_PLUGINS", OsString::from(PLUGIN_ID)),
    ];
    let mut crash_env = base_env.clone();
    crash_env.push(("CALM_TEST_CRASH_AT", OsString::from(CRASH_POINT)));

    // ---- boot#1: crash seam armed ------------------------------------------
    let Some(mut boot1) = launch_kernel(&tmp_path, &db_path, "boot-1", &crash_env) else {
        return; // sandbox denied loopback bind — CI-safe skip
    };
    assert_ne!(boot1.port, 4040);

    let socket_path = tmp_path.join("data").join("mcp").join("kernel.sock");
    let repo_arg = origin_repo.display().to_string();
    let head = "slice-840-e2-merge-crash";

    // Branch + commit + push directly in the lease worktree (no forge op
    // needed for setup), then open the PR through the kernel MCP socket.
    run_git(&seeded.lease_abs, ["checkout", "-b", head]);
    stage_git_change(&seeded.lease_abs, "merge-crash.txt", "merge crash e2\n");
    run_git(&seeded.lease_abs, ["commit", "-m", "merge crash e2"]);
    let head_sha = run_git_capture(&seeded.lease_abs, ["rev-parse", "HEAD"]);
    run_git(&seeded.lease_abs, ["push", "-u", "origin", head]);

    let create_resp = call_tool_via_socket(
        &socket_path,
        &seeded.raw_token,
        &seeded.thread_id,
        21,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": head,
            "base": "main",
            "title": "Merge crash reboot E2E",
            "body": "Created by #840 e2 merge-crash reboot test"
        }),
    )
    .await;
    assert_tool_succeeded(&create_resp, "gh.pr.create");
    let opened = wait_for_event_rows(repo.pool(), "forge.pr.opened", 1, ORACLE_TIMEOUT).await;
    let pr_number = opened[0]["pr_number"].as_u64().expect("pr number");

    // Send the merge WITHOUT awaiting the reply: the abort races the response
    // write, so a reply-reading client could see EOF/timeout and panic for the
    // wrong reason. Hold the connection open across the crash window.
    let _merge_conn = send_tool_call_without_reply(
        &socket_path,
        &seeded.raw_token,
        &seeded.thread_id,
        22,
        PR_MERGE_TOOL,
        json!({
            "repo": repo_arg,
            "pr": pr_number,
            "phase": "impl",
            "slice_id": "840"
        }),
    )
    .await;
    let merge_idem_key = format!(
        "{PLUGIN_ID}:{}:{}:gh.pr.merge:{repo_arg}:{pr_number}",
        seeded.wave_id, seeded.card_id
    );
    let op_id = wait_for_operation_id(repo.pool(), &merge_idem_key, ORACLE_TIMEOUT).await;

    // ---- the crash: SIGABRT from the seam, nothing else --------------------
    // Anti-vacuity: a clean exit, any other signal, or a 30s survival all fail
    // here — the seam demonstrably fired in the harness-spawned binary.
    let status = wait_exit_with_timeout(&mut boot1, CRASH_TIMEOUT);
    assert_eq!(
        status.signal(),
        Some(libc::SIGABRT),
        "boot#1 must die by the CALM_TEST_CRASH_AT abort seam, got {status:?}"
    );

    // ---- crash-window oracle (kernel dead; direct file-DB + shim state) ----
    // The fence tx must have rolled back: op still parked, zero merged events —
    // while the irreversible action itself demonstrably ran exactly once
    // (durable result file present with exit code 0, shim merge counter == 1).
    assert_eq!(
        query_operation_phase(repo.pool(), &op_id).await,
        "parked",
        "crash window: the pre-commit abort must leave the merge op parked"
    );
    assert_eq!(
        query_event_rows(repo.pool(), "forge.pr.merged").await.len(),
        0,
        "crash window: the uncommitted fence tx must take its decision event down with it"
    );
    let result_path = operation_result_path(repo.pool(), &op_id).await;
    assert!(
        result_path.starts_with(tmp_path.join("data")),
        "forge result file must live inside the throwaway data dir: {}",
        result_path.display()
    );
    let code_path = PathBuf::from(format!("{}.code", result_path.display()));
    let code = std::fs::read_to_string(&code_path)
        .unwrap_or_else(|e| panic!("crash window: {} must exist ({e})", code_path.display()));
    assert_eq!(
        code.trim(),
        "0",
        "crash window: the merge action must have run to completion before the abort"
    );
    let shim_state = PathBuf::from(format!("{repo_arg}.shimstate"));
    assert_eq!(
        shim_counter(&shim_state.join("pr_merge_count")),
        1,
        "crash window: gh must have merged exactly once before the abort"
    );

    // ---- boot#2: same tempdir, seam unarmed ---------------------------------
    // Recovery runs synchronously before the HTTP listener binds, so ready ⇒
    // recovery done; poll-with-timeout anyway for robustness.
    let Some(mut boot2) = launch_kernel(&tmp_path, &db_path, "boot-2", &base_env) else {
        return;
    };
    wait_for_operation_phase(repo.pool(), &op_id, "succeeded", ORACLE_TIMEOUT).await;
    let merged = wait_for_event_rows(repo.pool(), "forge.pr.merged", 1, ORACLE_TIMEOUT).await;
    boot2.sigkill_and_reap();

    // ---- final oracle: exactly-once merge across abort + reboot ------------
    assert_eq!(
        query_operation_phase(repo.pool(), &op_id).await,
        "succeeded"
    );
    let merge_ops: i64 = query_scalar_retry(
        repo.pool(),
        "SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1",
        &merge_idem_key,
    )
    .await;
    assert_eq!(
        merge_ops, 1,
        "exactly one operations row for the merge idempotency key"
    );
    assert_eq!(
        merged.len(),
        1,
        "exactly ONE forge.pr.merged event across abort + reboot"
    );
    let payload = &merged[0];
    assert_eq!(payload["wave_id"], json!(seeded.wave_id));
    assert_eq!(payload["head_sha"], json!(head_sha));
    assert_eq!(payload["subject"]["pr_number"], json!(pr_number));
    let merge_sha = payload["merge_sha"].as_str().expect("merge_sha string");
    assert!(
        merge_sha.len() == 40 && merge_sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "merge_sha should be a git-shaped oid: {merge_sha}"
    );
    assert_eq!(
        shim_counter(&shim_state.join("pr_merge_count")),
        1,
        "recovery must replay the durable result file, never re-run gh"
    );
    assert_eq!(
        query_event_rows(repo.pool(), "forge.pr.merged").await.len(),
        1,
        "forge.pr.merged count must still be exactly one after boot#2 was reaped"
    );
}

// ---------------------------------------------------------------------------
// World seeding
// ---------------------------------------------------------------------------

struct Seeded {
    wave_id: String,
    card_id: String,
    raw_token: String,
    thread_id: String,
    lease_abs: PathBuf,
}

/// Mirror of forge_workflow_e2e's `boot_fixture`/`create_worker_caller`
/// seeding, against the durable file DB: cove + wave (cwd = git clone of the
/// bare origin), Worker card keeping its `raw_token`, runtime + thread
/// binding, and a `held` workspace lease with **`boot_id` NULL** (the boot
/// reclaim predicate only reclaims when BOTH boot_ids are non-NULL and
/// unequal, so a NULL lease is never reclaimed) and a generous
/// `lease_until_ms = now + 1h` (don't depend on the TTL being unchecked).
async fn seed_world(repo: &Arc<SqlxRepo>, wave_cwd: &Path) -> Seeded {
    let as_repo: Arc<dyn Repo> = repo.clone();
    let cove = as_repo
        .cove_create(NewCove {
            name: "merge-crash-e2".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = as_repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "merge-crash-e2".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");

    let card_role_cache = CardRoleCache::new();
    let card_id = calm_server::model::new_id();
    let runtime_id = calm_server::model::new_id();
    let lease_abs = wave_cwd
        .join(".claude")
        .join("worktrees")
        .join(wave.id.as_str())
        .join(&card_id);

    let mut tx = repo.pool().begin().await.expect("begin card tx");
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &runtime_id,
        None,
        wave.id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        &card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint worker card");
    let raw_token = match mcp_token {
        Some(token) => token,
        None => {
            let token = calm_server::mcp_server::auth::CardMcpToken::generate();
            let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
            card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
                .await
                .expect("mint card MCP token");
            session_mcp_token_set_tx(&mut tx, &runtime_id, &token_hash)
                .await
                .expect("mint session MCP token");
            token.into_inner()
        }
    };
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', 'e2-test-lease-owner', ?5, NULL, ?6, ?6)"#,
    )
    .bind(calm_server::model::new_id())
    .bind(&card_id)
    .bind(wave.id.as_str())
    .bind(lease_abs.display().to_string())
    .bind(now + 3_600_000)
    .bind(now)
    .execute(&mut *tx)
    .await
    .expect("insert workspace lease");
    tx.commit().await.expect("commit card tx");

    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(repo, card_id.as_str(), thread_id.as_str()).await;

    Seeded {
        wave_id: wave.id.to_string(),
        card_id,
        raw_token,
        thread_id,
        lease_abs,
    }
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    let mut tx = repo.pool().begin().await.expect("begin runtime tx");
    if let Some(runtime) = session_projection_active_for_card_tx(&mut tx, card_id)
        .await
        .expect("active runtime lookup")
    {
        session_bind_attribution_tx(
            &mut tx,
            &runtime.id,
            ThreadAttribution {
                runtime_id: runtime.id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .expect("bind thread attribution");
    } else {
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: calm_server::model::new_id(),
                card_id: card_id.to_string(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .expect("start runtime");
    }
    tx.commit().await.expect("commit runtime tx");
}

/// Install the git-forge plugin the way the REAL boot loads it: an install
/// dir under `CALM_PLUGINS_DIR` (`<tmp>/plugins/<id>/{manifest.json,bin/…}`)
/// for `PluginRegistry::load_from_dir`, plus an enabled `plugins` DB row for
/// `PluginHost::autospawn_enabled`. The plugin binary exits on stdin EOF when
/// the kernel dies, so the abort leaks no orphan.
fn install_git_forge_plugin_files(tmp: &Path) {
    let install_dir = tmp.join("plugins").join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::copy(manifest_path(), install_dir.join("manifest.json"))
        .expect("copy git-forge manifest");
    std::os::unix::fs::symlink(Path::new(FORGE_BIN), bin_dir.join("git-forge"))
        .expect("symlink git-forge plugin");
}

async fn seed_plugin_row(repo: &Arc<SqlxRepo>, tmp: &Path) {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    let manifest = Manifest::parse(&raw).expect("git-forge manifest parses");
    let as_repo: Arc<dyn Repo> = repo.clone();
    as_repo
        .plugin_install(NewPlugin {
            id: PLUGIN_ID.into(),
            version: "0.1.0".into(),
            install_path: tmp.join("plugins").join(PLUGIN_ID).display().to_string(),
            manifest: manifest.to_json(),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-forge/manifest.json")
}

fn provision_worker_worktree(repo: &Path, wave_id: &str, card_id: &str, target: &Path) {
    ensure_worktree_root_excluded(repo);
    let parent = target.parent().expect("worker worktree target parent");
    std::fs::create_dir_all(parent).expect("create worker worktree parent");
    let branch = format!("neige/{wave_id}/{card_id}");
    run_git(
        repo,
        [
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            target.to_str().expect("utf-8 worktree path"),
        ],
    );
    configure_repo_identity(target);
}

fn ensure_worktree_root_excluded(repo: &Path) {
    use std::io::Write as _;

    const WORKTREE_EXCLUDE: &str = ".claude/worktrees/";
    let exclude = run_git_capture(repo, ["rev-parse", "--git-path", "info/exclude"]);
    let exclude = repo.join(exclude);
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == WORKTREE_EXCLUDE) {
        return;
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).expect("create git exclude parent");
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)
        .expect("open git exclude");
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file).expect("separate git exclude entries");
    }
    writeln!(file, "{WORKTREE_EXCLUDE}").expect("write worktree exclude");
}

// ---------------------------------------------------------------------------
// BUSY-tolerant polling oracles (boot#1/boot#2 are live WAL writers on the
// same file DB — treat transient query errors as "not yet", panic with the
// last error only at the deadline).
// ---------------------------------------------------------------------------

async fn query_scalar_retry<T>(pool: &SqlitePool, sql: &str, bind: &str) -> T
where
    T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    let deadline = Instant::now() + ORACLE_TIMEOUT;
    loop {
        match sqlx::query_scalar::<_, T>(sql)
            .bind(bind)
            .fetch_one(pool)
            .await
        {
            Ok(value) => return value,
            Err(e) => {
                if Instant::now() > deadline {
                    panic!("query `{sql}` kept failing: {e}");
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
}

async fn query_operation_phase(pool: &SqlitePool, op_id: &str) -> String {
    query_scalar_retry(pool, "SELECT phase FROM operations WHERE id = ?1", op_id).await
}

async fn wait_for_operation_phase(
    pool: &SqlitePool,
    op_id: &str,
    expected: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("<query never succeeded>");
    loop {
        if let Ok(phase) =
            sqlx::query_scalar::<_, String>("SELECT phase FROM operations WHERE id = ?1")
                .bind(op_id)
                .fetch_one(pool)
                .await
        {
            if phase == expected {
                return;
            }
            last = phase;
        }
        if Instant::now() > deadline {
            panic!("expected operation {op_id} phase `{expected}`, last saw `{last}`");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_operation_id(
    pool: &SqlitePool,
    idempotency_key: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(id)) =
            sqlx::query_scalar::<_, String>("SELECT id FROM operations WHERE idempotency_key = ?1")
                .bind(idempotency_key)
                .fetch_optional(pool)
                .await
        {
            return id;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for operation with idempotency_key `{idempotency_key}`");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn query_event_rows(pool: &SqlitePool, kind: &str) -> Vec<Value> {
    let deadline = Instant::now() + ORACLE_TIMEOUT;
    loop {
        match sqlx::query_scalar::<_, String>(
            "SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC",
        )
        .bind(kind)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => {
                return rows
                    .into_iter()
                    .map(|raw| serde_json::from_str(&raw).expect("event payload json"))
                    .collect();
            }
            Err(e) => {
                if Instant::now() > deadline {
                    panic!("event rows query for `{kind}` kept failing: {e}");
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
}

async fn wait_for_event_rows(
    pool: &SqlitePool,
    kind: &str,
    expected: usize,
    timeout: Duration,
) -> Vec<Value> {
    let deadline = Instant::now() + timeout;
    let mut last = 0usize;
    loop {
        if let Ok(rows) = sqlx::query_scalar::<_, String>(
            "SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC",
        )
        .bind(kind)
        .fetch_all(pool)
        .await
        {
            last = rows.len();
            if rows.len() == expected {
                return rows
                    .into_iter()
                    .map(|raw| serde_json::from_str(&raw).expect("event payload json"))
                    .collect();
            }
        }
        if Instant::now() > deadline {
            panic!("expected {expected} `{kind}` events, last saw {last}");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn operation_result_path(pool: &SqlitePool, op_id: &str) -> PathBuf {
    let raw: String = query_scalar_retry(
        pool,
        "SELECT tx_output_json FROM operations WHERE id = ?1",
        op_id,
    )
    .await;
    let output: Value = serde_json::from_str(&raw).expect("tx_output_json parses");
    PathBuf::from(
        output["data"]["result_path"]
            .as_str()
            .expect("forge result_path in tx_output"),
    )
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn assert_tool_succeeded(resp: &Value, label: &str) {
    assert!(
        resp.get("error").is_none(),
        "{label} returned JSON-RPC error: {resp:#?}"
    );
    assert_eq!(
        resp["result"]["isError"], false,
        "{label} returned MCP tool error: {resp:#?}"
    );
    assert!(
        resp["result"]["structuredContent"]["op_id"]
            .as_str()
            .is_some(),
        "{label} response must carry op_id: {resp:#?}"
    );
}

fn shim_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn prepend_to_path(dir: &Path) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut value = OsString::from(dir.as_os_str());
    value.push(OsStr::new(":"));
    value.push(current);
    value
}

/// The kernel derives its MCP UDS at `<tempdir>/data/mcp/kernel.sock`, and
/// `sockaddr_un` caps paths at ~108 bytes — if the ambient temp dir is deeply
/// nested (long `TMPDIR`), fall back to literal `/tmp` (still within the
/// prod-safety guard's allowed roots).
fn socket_safe_tempdir() -> std::io::Result<TempDir> {
    let ambient = std::env::temp_dir();
    let base = if ambient.as_os_str().len() <= 40 {
        ambient
    } else {
        PathBuf::from("/tmp")
    };
    tempfile::Builder::new().prefix("e2mc").tempdir_in(base)
}
