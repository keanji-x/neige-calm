//! Issue #1147 S3 — changing a wave's workspace, driven through the real
//! `PATCH /api/waves/{id}`.
//!
//! Every test here runs the production route and then **looks at the
//! filesystem**. That discipline is inherited from S4/S5: the assertion that
//! actually stops the accident is "perform the operation for real, then go and
//! see what is on disk", not a SQL query over `workspace_path`.
//!
//! The three-step execution shape (design §更换与冻结) each has its own test:
//!
//! 1. the fence — `the_fence_is_up_before_the_move_not_after_it`. Note the
//!    name: asserting the END state proves nothing, because the harness
//!    restart supersedes the old runtime on its own. Measured.
//! 2. the pre-move re-check — `a_write_between_the_fence_and_the_move_is_refused`
//!    (the ONE timing predicate in this design; a static state assertion
//!    cannot stand in for it)
//! 3. the move's own assertions — inherited from S5's
//!    `recycle_wave_workspace`, exercised here end to end
//!
//! **Running a subset locally: pass `--no-fail-fast`.** Without it cargo stops
//! at the first failure, and a mutation check that stops at 48 of 62 reports
//! "only one test died" for a mutation that actually kills three. That is not
//! hypothetical — it nearly recorded N19's two new tests as one, in the
//! opposite direction from the mistake above: a wrong *green* rather than a
//! wrong *cause*.
//!
//! and the four refusals (frozen / already attached / system area / non-empty)
//! each have one too. The freeze latch's system-area exclusion has
//! `a_workspace_lease_never_freezes_the_launchpad`; freeze point 2 (terminal
//! persistence), which S3 left open as gap N17 and S6 closed, has
//! `a_terminal_card_lands_in_the_workspace_and_freezes_it`.
//!
//! The transition is `managed → attached` and nothing else. There is no
//! `managed → managed`: a managed path is derived from the wave's area and id,
//! so re-allocating one always re-derives the same directory
//! (`a_managed_target_is_a_documented_400_not_a_silent_no_op`).

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::FromRef;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::wave_area_cache::WaveAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness (same shape as `wave_workspace_recycle.rs`)
// ---------------------------------------------------------------------------

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    /// #1147 S3 — the registry the route's in-memory fence acts on.
    ///
    /// Held so tests can install a REAL `SpecHarness` into it. Without one the
    /// whole `harness.get` / `shutdown` / `remove` half of the fence is dead
    /// code under test: it was measured that deleting it turned no test red,
    /// which is exactly the shape this slice has been caught by twice.
    harness: calm_server::harness::HarnessRegistry,
    /// Kept so a test can call an operation directly, for guards no HTTP
    /// request can reach — see `a_non_user_actor_may_not_change_a_workspace`.
    state: AppState,
    roles: CardRoleCache,
    waves: WaveAreaCache,
    shared_codex: Arc<calm_server::shared_codex_appserver::SharedCodexAppServer>,
    #[allow(dead_code)]
    tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let roles = CardRoleCache::new();
    let waves = WaveAreaCache::new();
    sqlx_repo.seed_card_role_cache(&roles).await.unwrap();
    sqlx_repo.seed_wave_area_cache(&waves).await.unwrap();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), waves.clone()),
    ));
    let shared_codex = SharedCodexAppServer::new_fake_running_with_pending(sqlx_repo.clone(), None);
    let state = AppState::from_parts(
        repo,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles.clone()),
        Some(waves.clone()),
    )
    .with_shared_codex_appserver(shared_codex.clone())
    .with_workspace_root(workspace_root.clone());
    let harness = state.harness.clone();
    let state_for_tests = state.clone();
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo: sqlx_repo,
        workspace_root,
        harness,
        state: state_for_tests,
        roles,
        waves,
        shared_codex,
        tmp,
    }
}

/// Install a real `SpecHarness` in the registry under the wave's live
/// spec-harness runtime, and return that runtime id.
///
/// `run_unstarted_for_test` builds the handle without spawning the run loop, so
/// the test gets a genuine `SpecHarness` — one whose `shutdown()` really runs —
/// with no background task to race the assertions.
async fn install_live_harness(b: &Boot, wave_id: &str) -> String {
    let runtime_id: String = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id LIMIT 1",
    )
    .bind(wave_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let card_id: String = sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id=?1")
        .bind(&runtime_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let repo: Arc<dyn Repo> = b.repo.clone();
    let (harness, _observations) = calm_server::harness::SpecHarness::run_unstarted_for_test(
        calm_server::harness::SpecHarnessParams {
            runtime_id: runtime_id.clone(),
            wave_id: wave_id.to_string().into(),
            card_id: card_id.into(),
            thread_id: None,
            repo,
            events: calm_server::event::EventBus::new(),
            card_role_cache: b.roles.clone(),
            wave_area_cache: b.waves.clone(),
            daemon: b.shared_codex.clone(),
            config: Default::default(),
            snapshot: calm_server::harness::HarnessSnapshot::initial(0, Vec::new()),
        },
        8,
    );
    b.harness.insert(runtime_id.clone(), harness);
    assert!(
        b.harness.get(&runtime_id).is_some(),
        "premise: the registry now holds a live harness for {runtime_id}"
    );
    runtime_id
}

fn theme() -> Value {
    json!({"fg": [255, 255, 255], "bg": [0, 0, 0]})
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn create_area(b: &Boot, name: &str) -> String {
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/areas",
        Some(json!({"name": name, "color": "#abc"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let area: Value = serde_json::from_str(&body).unwrap();
    area["id"].as_str().unwrap().to_string()
}

/// A managed wave: the title-only create the new FE sends.
async fn managed_wave(b: &Boot, area_id: &str, title: &str) -> (String, PathBuf) {
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({"area_id": area_id, "title": title, "theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let wave: Value = serde_json::from_str(&text).unwrap();
    let id = wave["id"].as_str().unwrap().to_string();
    let path = workspace_path(b, &id).await;
    assert!(
        path.join(".git").is_dir(),
        "expected a materialized repository at {path:?}"
    );
    (id, path)
}

async fn workspace_path(b: &Boot, wave_id: &str) -> PathBuf {
    let path: String = sqlx::query_scalar("SELECT workspace_path FROM waves WHERE id=?1")
        .bind(wave_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    PathBuf::from(path)
}

async fn workspace_row(b: &Boot, wave_id: &str) -> (String, String, Option<i64>) {
    sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id=?1",
    )
    .bind(wave_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap()
}

/// `PATCH /api/waves/{id}` pointing a wave at an existing repository.
async fn repoint_to(b: &Boot, wave_id: &str, path: &Path) -> (StatusCode, String) {
    request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave_id}"),
        Some(json!({"workspace": {
            "kind": "attached",
            "path": path.to_string_lossy(),
            "attach_folder": true,
        }})),
    )
    .await
}

/// The refusal tests do not care *where* the wave would have gone, only that
/// it does not go: they all use a perfectly valid target so the refusal cannot
/// be coming from target validation.
async fn repoint(b: &Boot, wave_id: &str) -> (StatusCode, String) {
    let target = user_repo(&b.tmp.path().join(format!("target-{wave_id}")));
    repoint_to(b, wave_id, &target).await
}

/// Every `spec-harness-start` payload submitted so far, oldest first.
async fn harness_start_payloads(b: &Boot) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind='spec-harness-start' \
         ORDER BY created_at_ms, id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap()
}

fn trash_entries(workspace_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(workspace_root.join(".trash")) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    out.sort();
    out
}

fn git(at: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} in {at:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn with_identity(at: &Path) {
    git(at, &["config", "user.name", "fixture"]);
    git(at, &["config", "user.email", "fixture@example.com"]);
}

fn commit_count(path: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-list", "--count", "--all"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Every path under `root`, with file contents. Directories map to `None`,
/// symlinks to their target, files to their exact bytes.
///
/// Comparing this before and after is the assertion that matters for an
/// attached target: not "the directory still exists", but "not one byte
/// moved". It deliberately includes `.git/` — a `.claude/worktrees/` line
/// appearing in `.git/info/exclude`, or a `neige-workspace` marker showing up,
/// are both ways the server could have taken ownership of a user's repository,
/// and both show up here as a diff.
fn fingerprint(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    /// Git's **own** transient lock files under `.git/`, by exact name.
    ///
    /// Measured on CI (git 2.55) and not reproducible on this host (git 2.39):
    /// background maintenance creates `.git/objects/maintenance.lock` after a
    /// commit and removes it moments later, so a before/after pair straddling
    /// that window reports `removed: …maintenance.lock` and blames the server
    /// for a file it never saw. `user_repo` also turns maintenance off; this is
    /// the half that does not depend on remembering to.
    ///
    /// **Named, and rooted at `.git/`, on purpose.** The first version matched
    /// any `*.lock` anywhere, which was strictly wrong: it blinded the
    /// fingerprint to `.git/config.lock` and `.git/index.lock` — the exact
    /// files `workspace_materialize::clear_our_stale_git_locks` deletes — so
    /// the one production routine that removes files from a repository became
    /// invisible to the assertion whose entire job is "the server did not touch
    /// the user's repository". It also hid a work-tree `Cargo.lock`. An
    /// unexpected `*.lock` must fail this assertion, not be waved through.
    fn is_transient_git_lock(rel: &Path) -> bool {
        const NAMES: [&str; 2] = ["maintenance.lock", "gc.pid.lock"];
        rel.starts_with(".git")
            && rel
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| NAMES.contains(&name))
    }
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if is_transient_git_lock(&rel) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                out.insert(rel, Some(target.into_os_string().into_encoded_bytes()));
            } else if meta.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn diff(
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    after: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (path, bytes) in before {
        match after.get(path) {
            None => out.push(format!("removed: {}", path.display())),
            Some(other) if other != bytes => out.push(format!("changed: {}", path.display())),
            Some(_) => {}
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            out.push(format!("added: {}", path.display()));
        }
    }
    out
}

/// A user-owned repository outside the managed root.
fn user_repo(at: &Path) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    git(at, &["init", "-b", "main"]);
    with_identity(at);
    // Keep git from touching this repository behind our back. Since 2.5x a
    // commit can kick off background maintenance, which leaves
    // `.git/objects/maintenance.lock` around for a moment — long enough for a
    // fingerprint pair to straddle it and blame the server. Measured on CI
    // (git 2.55); this host runs 2.39 and never showed it, which is the whole
    // reason it reached CI.
    git(at, &["config", "gc.auto", "0"]);
    git(at, &["config", "maintenance.auto", "false"]);
    std::fs::write(at.join("README.md"), b"the user's own work\n").unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-q", "--no-verify", "-m", "user commit"]);
    at.to_path_buf()
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_pristine_wave_is_pointed_at_the_users_repository() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, managed_path) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    let target_before = fingerprint(&target);

    // Something we can recognise later, invisible to the emptiness predicate
    // because it lives inside `.git/`.
    std::fs::write(managed_path.join(".git").join("fixture-witness"), b"old\n").unwrap();
    assert!(trash_entries(&b.workspace_root).is_empty());

    let (status, body) = repoint_to(&b, &wave, &target).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // The row points at the user's repository, attached and frozen.
    let (kind, path, frozen) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(&path), target);
    assert!(
        frozen.is_some(),
        "`attached -> *` is not a legal transition, so this is a one-way door and \
         the row must say so; S4's `no_attached_wave_is_ever_unfrozen` pins the \
         same thing over the whole table"
    );

    // The user's repository is byte-for-byte untouched: not initialized, not
    // committed into, no `.git/info/exclude` line, no ownership marker.
    assert_eq!(
        diff(&target_before, &fingerprint(&target)),
        Vec::<String>::new(),
        "the server must not have touched the user's repository"
    );

    // The OLD managed directory is in the trash — moved, not deleted.
    let trashed = trash_entries(&b.workspace_root);
    assert_eq!(trashed.len(), 1, "expected exactly one trash entry");
    assert!(
        trashed[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&wave),
        "trash entry {trashed:?} should be named after the wave"
    );
    assert_eq!(
        std::fs::read_to_string(trashed[0].join(".git").join("fixture-witness")).unwrap(),
        "old\n",
        "the old workspace must be MOVED, not deleted"
    );
    assert!(
        !managed_path.exists(),
        "the old managed directory must be gone from its original path"
    );

    // The claim was minted for this area, so a second wave in the same
    // repository does not have to re-argue ownership.
    let claims: Vec<(String, String)> = sqlx::query_as("SELECT path, area_id FROM area_folders")
        .fetch_all(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        claims,
        vec![(target.to_string_lossy().into_owned(), area.clone())],
        "`attach_folder: true` must claim the directory for the wave's area"
    );
}

/// Frozen means frozen: the door only opens once.
#[tokio::test]
async fn a_second_repoint_is_refused_because_the_first_one_froze_it() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    let first = user_repo(&b.tmp.path().join("first"));
    let second = user_repo(&b.tmp.path().join("second"));

    let (status, body) = repoint_to(&b, &wave, &first).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let (status, body) = repoint_to(&b, &wave, &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        PathBuf::from(workspace_row(&b, &wave).await.1),
        first,
        "the wave must still point at the first repository"
    );
}

#[tokio::test]
async fn the_spec_harness_is_restarted_on_the_new_path_with_a_new_thread() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, managed_path) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));

    let (status, body) = repoint_to(&b, &wave, &target).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // The re-point must submit a `spec-harness-start` carrying the NEW cwd and
    // `force_new_thread: true`. A resumed thread keeps the cwd it was minted
    // with, so `force_new_thread: false` would leave the spec agent working in
    // the trashed directory while every worker uses the new one.
    let payloads = harness_start_payloads(&b).await;
    let last: Value = serde_json::from_str(payloads.last().expect("a harness start")).unwrap();
    assert_eq!(
        last["cwd"].as_str(),
        Some(target.to_string_lossy().as_ref()),
        "the restart must carry the wave's NEW workspace path, not the trashed \
         one; payload={last}"
    );
    assert_ne!(
        last["cwd"].as_str(),
        Some(managed_path.to_string_lossy().as_ref()),
        "payload={last}"
    );
    assert_eq!(
        last["force_new_thread"],
        json!(true),
        "payload={last} — a re-point must mint a new thread"
    );
    assert_eq!(
        last["reset_harness_items"],
        json!(false),
        "payload={last} — harness items are persisted per card, so re-opening \
         the thread must not wipe the user's transcript"
    );
}

/// The fence must be up **before** the move, not merely as a side effect of
/// the restart afterwards.
///
/// Asserting the end state proves nothing: the restart carries
/// `force_new_thread: true`, which supersedes the previous runtime on its own
/// (`session_prepare_deferred_spec_tx`), so an after-the-fact check is green
/// even with the fence deleted — measured. The question is a *temporal* one —
/// "could a push still start a turn at the moment the directory moves?" — so
/// it is asked in the window, through the same hook the re-check test uses.
#[tokio::test]
async fn the_fence_is_up_before_the_move_not_after_it() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));

    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: a freshly created wave has an active spec-harness runtime, \
         otherwise this test cannot observe the fence at all"
    );

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::waves::install_workspace_repoint_race_hook_for_test(
        &wave,
        calm_server::routes::waves::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let app = b.app.clone();
    let wave_for_task = wave.clone();
    let target_for_task = target.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/waves/{wave_for_task}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target_for_task.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await
    });

    entered.notified().await;
    // In the window: the fence transaction has committed and nothing has moved
    // yet. Every runtime that was active must already be gone from the set
    // `dispatcher::harness_runtime_id_for_spec_card` reads.
    let still_active: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    release.notify_one();
    let (status, body) = patch.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        still_active.is_empty(),
        "runtimes {still_active:?} were still active while the workspace was \
         about to be renamed into the trash. \
         `session_projection_active_for_card` is what the dispatcher consults \
         before delivering an observation, so an active row in this window means \
         a push could start a turn whose writes land in `.trash`."
    );
}

// ---------------------------------------------------------------------------
// Step 2 — the ONE timing predicate
// ---------------------------------------------------------------------------

/// A write that lands **after** the criteria transaction committed and
/// **before** the move must abort the re-point.
///
/// This is the test the design's "SQLite 事务不能隔离文件系统写入" sentence
/// exists for, and it cannot be replaced by a static assertion: at the moment
/// the fence transaction commits, every durable state the route can read says
/// the workspace is empty. Deleting the pre-move re-check leaves this test —
/// and only this test — red.
#[tokio::test]
async fn a_write_between_the_fence_and_the_move_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::waves::install_workspace_repoint_race_hook_for_test(
        &wave,
        calm_server::routes::waves::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let app = b.app.clone();
    let wave_for_task = wave.clone();
    let target_for_task = target.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/waves/{wave_for_task}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target_for_task.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await
    });

    // The racing writer: exactly what a turn that was already in flight when
    // the fence went up would do.
    entered.notified().await;
    std::fs::write(
        path.join("agent-output.md"),
        b"the turn was still running\n",
    )
    .unwrap();
    release.notify_one();

    let (status, body) = patch.await.unwrap();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write in the fence→move window must abort the re-point; body={body}"
    );

    // Nothing moved, and nothing was lost.
    assert!(
        trash_entries(&b.workspace_root).is_empty(),
        "the workspace must not have been renamed into the trash"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("agent-output.md")).unwrap(),
        "the turn was still running\n",
        "the racing turn's output must still be where it was written"
    );
    assert_eq!(
        workspace_path(&b, &wave).await,
        path,
        "the stored path must be unchanged after a refusal"
    );
    // …and no claim was minted. The fence transaction COMMITS (it is also what
    // supersedes the runtimes), so its claim pass has to be scan-only: a row
    // written there would outlive this refusal and leave the caller a 409 plus
    // a `area_folders` claim they never got a wave for.
    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        claims, 0,
        "a refusal after the fence must leave no folder claim behind"
    );
}

// ---------------------------------------------------------------------------
// The "anything on disk" refusals — one per clause, through the real route
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_plain_file_in_the_workspace_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    std::fs::write(path.join("notes.md"), b"the agent wrote this\n").unwrap();

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("notes.md")).unwrap(),
        "the agent wrote this\n"
    );
}

/// Worker output under `.claude/worktrees/` is EXCLUDED by
/// `.git/info/exclude`, so a plain `git status --porcelain` cannot see it.
/// Only the `--ignored` clause rejects this.
#[tokio::test]
async fn excluded_worker_output_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let lease = path
        .join(".claude")
        .join("worktrees")
        .join(&wave)
        .join("c1");
    std::fs::create_dir_all(&lease).unwrap();
    std::fs::write(lease.join("report.md"), b"worker output\n").unwrap();

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "ignored worker output must block the change; body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(lease.join("report.md").exists());
}

/// A commit on a slice branch leaves the working tree clean; only the
/// `rev-list --count --all` clause rejects it.
#[tokio::test]
async fn a_commit_on_a_slice_branch_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    with_identity(&path);
    git(&path, &["checkout", "-q", "-b", "neige/slice"]);
    std::fs::write(path.join("work.txt"), b"worker work\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "--no-verify", "-m", "work"]);
    git(&path, &["checkout", "-q", "main"]);

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(commit_count(&path), "2", "the commit must still be there");
}

/// A lease worktree at the path leases really use. Doubly covered — the
/// checkout inside `.claude/worktrees/` is also ignored-but-present, so the
/// status clause rejects it first — which is why the single-violation fixture
/// for the worktree clause is the test below, not this one. Kept because this
/// is the shape production actually produces.
#[tokio::test]
async fn a_lease_worktree_at_the_real_lease_path_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let lease = path
        .join(".claude")
        .join("worktrees")
        .join(&wave)
        .join("c1");
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "neige/lease",
            lease.to_str().unwrap(),
        ],
    );

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(lease.join(".git").exists());
}

/// A worktree whose files are **outside** the workspace: the repository here is
/// clean by every other measure, and only `git worktree list` says otherwise.
///
/// Moving this repository would dangle `<wt>/.git` and
/// `<repo>/.git/worktrees/<n>/gitdir` — two absolute pointers, in both
/// directions — so the copy in the trash would not even be a usable
/// repository. Single-violation fixture for the worktree clause: measured, the
/// clause's removal turns this test red and no other integration test.
#[tokio::test]
async fn a_worktree_outside_the_workspace_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let elsewhere = b.tmp.path().join("detached-worktree");
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "neige/detached",
            elsewhere.to_str().unwrap(),
        ],
    );

    // Premise: the other two clauses are blind to this.
    let status_out = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["status", "--porcelain", "--ignored"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status_out.stdout)
            .trim()
            .is_empty(),
        "premise broken: the status clause already sees this fixture"
    );
    assert_eq!(
        commit_count(&path),
        "1",
        "premise broken: a commit was added"
    );

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(elsewhere.join(".git").exists());
}

// ---------------------------------------------------------------------------
// The typed refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_attached_workspace_refuses_the_change() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo_dir = user_repo(&b.tmp.path().join("user-project"));
    let (status, text) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({
            "area_id": area,
            "title": "attached",
            "cwd": repo_dir.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={text}");
    let wave = serde_json::from_str::<Value>(&text).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "attached repositories belong to the user; body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(
        repo_dir.join("README.md").exists(),
        "the user's repository must not be touched"
    );
    let (kind, path, _) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), repo_dir);
}

/// The `kind` guard on its own, with the freeze latch taken out of the way.
///
/// The test above is doubly covered: `AttachedFromCwd` freezes at creation, so
/// the freeze guard rejects before the kind guard is reached — measured, and
/// deleting the kind guard turns no test red. That defence in depth is
/// correct, but it leaves the guard without a fixture, and the guard is the
/// one that decides whether the server may `rename` a directory a **user**
/// owns.
///
/// So the row is put into `attached` + unfrozen directly. That state is not
/// reachable through any route today — migration 0077's comment names it
/// exactly ("an unfrozen `attached` row is exactly the state in which a future
/// PATCH branch that forgot to check `kind` would relocate a real user
/// repository") — which is the point: this asserts the guard, not the
/// reachability. Same discipline as S5's guard-4 fixture.
#[tokio::test]
async fn an_unfrozen_attached_workspace_is_still_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo_dir = user_repo(&b.tmp.path().join("user-project"));
    let (wave, _) = managed_wave(&b, &area, "w").await;
    sqlx::query(
        "UPDATE waves SET workspace_kind='attached', workspace_path=?1, \
         workspace_frozen_at=NULL WHERE id=?2",
    )
    .bind(repo_dir.to_string_lossy().as_ref())
    .bind(&wave)
    .execute(b.repo.pool())
    .await
    .unwrap();
    let before = std::fs::read_to_string(repo_dir.join("README.md")).unwrap();

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an attached workspace must be refused on `kind` alone, with no freeze \
         stamp to fall back on; body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("README.md")).unwrap(),
        before,
        "the user's repository must not have been touched"
    );
    let (kind, path, _) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), repo_dir);
}

/// The system area's launchpad path is kernel-maintained and is the documented
/// exception to the freeze latch, so it must be unreachable from the PATCH
/// route — otherwise the exception becomes a hole.
#[tokio::test]
async fn a_system_area_wave_refuses_the_change() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "launchpad ensure failed: {status} {body}"
    );
    let launchpad: Value = serde_json::from_str(&body).unwrap();
    let wave = launchpad["wave_id"].as_str().unwrap().to_string();
    let before = workspace_row(&b, &wave).await;

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        workspace_row(&b, &wave).await,
        before,
        "the launchpad's workspace row must be untouched"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn a_managed_target_is_a_documented_400_not_a_silent_no_op() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({"workspace": {"kind": "managed", "path": path.to_string_lossy()}})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a managed path is derived from the wave, so `managed -> managed` would \
         always re-derive the same directory; answering it explicitly beats \
         letting a caller believe an in-place reset was a change. body={body}"
    );
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(workspace_row(&b, &wave).await.0, "managed");
}

// ---------------------------------------------------------------------------
// Target validation (design D3) — the same three checks on both routes
// ---------------------------------------------------------------------------

/// Failure has to surface HERE, with git's own words.
///
/// Without this, attaching a directory that does not exist is a 201, and the
/// first `kind: codex` task then dies inside `git_repo_root_for_wave_cwd`
/// leaving nothing but `spawn-failed` in `tasks.status_detail`. That is issue
/// #1147's opening paragraph — so accepting it from the entry point this slice
/// adds would have shipped the original defect through a new door.
#[tokio::test]
async fn attaching_a_path_that_does_not_exist_is_refused_on_both_routes() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let missing = b.tmp.path().join("no-such-directory");

    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({
            "area_id": area, "title": "w", "theme": theme(),
            "cwd": missing.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body.contains("does not exist"),
        "the response must say what is wrong, not `spawn-failed`: {body}"
    );
    let waves: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE title='w'")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(waves, 0, "a refused create must leave no wave row");

    let (wave, _) = managed_wave(&b, &area, "patched").await;
    let (status, body) = repoint_to(&b, &wave, &missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("does not exist"), "{body}");
    assert_eq!(workspace_row(&b, &wave).await.0, "managed");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn attaching_a_directory_that_is_not_a_git_work_tree_is_refused_on_both_routes() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    // A real directory, outside any repository, with no `.git`.
    let plain = b.tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("notes.txt"), b"just a folder\n").unwrap();

    // Premise, asserted rather than assumed: git discovery walks UPWARD, so
    // this test is only meaningful while no ANCESTOR of the temp dir is a work
    // tree. A stray `.git` in `$TMPDIR` — which really does turn up on shared
    // dev boxes — makes every tempdir "inside a repository" and turns the 400
    // below into a 201, which reads as "the guard is broken" when it is the
    // fixture's world that changed. Fail here instead, naming the cause.
    let discovery = Command::new("git")
        .arg("-C")
        .arg(&plain)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .unwrap();
    assert!(
        !discovery.status.success(),
        "premise broken: {plain:?} resolves to a Git work tree at {}. Some \
         ancestor of $TMPDIR contains a `.git` — remove it; this test needs a \
         directory that is genuinely outside every repository.",
        String::from_utf8_lossy(&discovery.stdout).trim()
    );

    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({
            "area_id": area, "title": "w", "theme": theme(),
            "cwd": plain.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body.contains("not inside a Git work tree"),
        "the response must carry the real reason: {body}"
    );

    let (wave, _) = managed_wave(&b, &area, "patched").await;
    let (status, body) = repoint_to(&b, &wave, &plain).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("not inside a Git work tree"), "{body}");
    assert_eq!(workspace_row(&b, &wave).await.0, "managed");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

#[tokio::test]
async fn attaching_a_file_rather_than_a_directory_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let file = b.tmp.path().join("a-file");
    std::fs::write(&file, b"not a directory\n").unwrap();
    let (wave, _) = managed_wave(&b, &area, "w").await;

    let (status, body) = repoint_to(&b, &wave, &file).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("is not a directory"), "{body}");
}

/// A subdirectory of a repository is a legal cwd, deliberately.
///
/// `rev-parse --show-toplevel` succeeds there, and the worker path derives the
/// repository root itself (`git_repo_root_for_wave_cwd`) — so refusing it
/// would reject a directory work can actually happen in, for a reason nothing
/// downstream cares about.
#[tokio::test]
async fn attaching_a_subdirectory_of_a_repository_is_allowed() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));
    let sub = repo.join("crates");
    std::fs::create_dir_all(&sub).unwrap();
    let (wave, _) = managed_wave(&b, &area, "w").await;

    let (status, body) = repoint_to(&b, &wave, &sub).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(PathBuf::from(workspace_row(&b, &wave).await.1), sub);
}

// ---------------------------------------------------------------------------
// The area_folders claim rules — the same ones `POST /api/waves` uses
// ---------------------------------------------------------------------------

/// A directory another area already claims comes back as the STRUCTURED 409,
/// with nothing moved. Same body shape the create route returns, because both
/// go through `enforce_folder_claim_tx`.
#[tokio::test]
async fn a_directory_claimed_by_another_area_is_a_structured_conflict() {
    let b = boot().await;
    let owner = create_area(&b, "owner").await;
    let other = create_area(&b, "other").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));

    // `owner` claims it first, through the create route.
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({
            "area_id": owner, "title": "first", "theme": theme(),
            "cwd": repo.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (wave, managed_path) = managed_wave(&b, &other, "w").await;
    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: the wave has a live spec harness"
    );

    let (status, body) = repoint_to(&b, &wave, &repo).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    let conflict: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        conflict["area_id"].as_str(),
        Some(owner.as_str()),
        "the 409 must name the area that owns the directory, not just say \
         `conflict`: {body}"
    );
    assert_eq!(
        conflict["conflict_path"].as_str(),
        Some(repo.to_string_lossy().as_ref())
    );
    assert!(conflict["conflict_kind"].is_string(), "{body}");

    // Nothing happened.
    let (kind, path, frozen) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "managed");
    assert_eq!(PathBuf::from(path), managed_path);
    assert_eq!(frozen, None);
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert!(managed_path.join(".git").is_dir());

    // …including the spec harness. The claim rules are checked in the fence
    // transaction *before* the supersede, so a target that was never going to
    // be accepted does not cost the user their running agent. Without that
    // early check the conflict is still caught (the write transaction re-runs
    // the same rules, authoritatively) but only after the harness has been
    // torn down and restarted — a worse answer to the same question.
    let active_after: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        active_after, active_before,
        "a refused target must leave the running spec harness alone"
    );
}

/// Without `attach_folder`, an unclaimed directory is refused rather than
/// silently making a homeless wave — the same rule `POST /api/waves` has.
#[tokio::test]
async fn an_unclaimed_directory_without_attach_folder_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));
    let (wave, _) = managed_wave(&b, &area, "w").await;

    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({"workspace": {"kind": "attached", "path": repo.to_string_lossy()}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(
        body.contains("attach_folder"),
        "the refusal must say how to proceed: {body}"
    );
    assert_eq!(workspace_row(&b, &wave).await.0, "managed");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

/// A directory this area already claims is a no-op for the claim table, not a
/// duplicate-row 409 — issue #275's rule, inherited for free.
#[tokio::test]
async fn a_directory_this_area_already_claims_needs_no_new_claim() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let repo = user_repo(&b.tmp.path().join("my-project"));
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/waves",
        Some(json!({
            "area_id": area, "title": "first", "theme": theme(),
            "cwd": repo.to_string_lossy(), "attach_folder": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (wave, _) = managed_wave(&b, &area, "second").await;
    let (status, body) = repoint_to(&b, &wave, &repo).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(claims, 1, "the second wave must not mint a duplicate claim");
}

#[tokio::test]
async fn a_workspace_change_cannot_ride_along_with_row_edits() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    // A target that would otherwise be accepted, so the 400 can only be coming
    // from the mixing rule.
    let target = user_repo(&b.tmp.path().join("my-project"));
    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({
            "title": "renamed",
            "workspace": {
                "kind": "attached",
                "path": target.to_string_lossy(),
                "attach_folder": true,
            },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let title: String = sqlx::query_scalar("SELECT title FROM waves WHERE id=?1")
        .bind(&wave)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(title, "w", "the row edit must not have been applied either");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

// ---------------------------------------------------------------------------
// The freeze latch — each freeze point through its real production route
// ---------------------------------------------------------------------------

/// Freeze point 2: terminal persistence — #1147 S6, closing gap N17.
///
/// This test REPLACES `a_terminal_card_does_not_freeze_the_workspace_yet_n17`,
/// which asserted the gap and the premise that made it harmless in S3 ("no
/// terminal ever captures `waves.workspace_path`"). S6 is the slice that makes
/// terminals land in the workspace, so that premise is gone and the gap is a
/// real hole; the old test is not relaxed here, it is inverted.
///
/// The two halves are asserted together on purpose. The freeze without the
/// default would be over-strict (a terminal that never captured the path would
/// still nail the workspace down); the default without the freeze is the hole —
/// a re-point would rename the terminal's directory into `.trash/` while a
/// `terminals` row still points at it, and nothing re-anchors a `terminals.cwd`.
#[tokio::test]
async fn a_terminal_card_lands_in_the_workspace_and_freezes_it() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    assert_eq!(workspace_row(&b, &wave).await.2, None, "premise: unfrozen");

    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/waves/{wave}/terminal-cards"),
        Some(json!({"theme": theme()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let terminal_cwds: Vec<String> = sqlx::query_scalar(
        "SELECT t.cwd FROM terminals t JOIN cards c ON c.id = t.card_id WHERE c.wave_id = ?1",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(!terminal_cwds.is_empty(), "a terminal row must exist");
    for cwd in &terminal_cwds {
        assert_eq!(
            PathBuf::from(cwd),
            path,
            "#1147 S6: a terminal card with no explicit cwd must open in the \
             wave's workspace, not in $HOME"
        );
    }

    assert!(
        workspace_row(&b, &wave).await.2.is_some(),
        "#1147 S6 freeze point 2: persisting a terminal row freezes the workspace"
    );
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the wave now has a durable cwd consumer; the re-point must be refused; body={body}"
    );
    assert_eq!(
        workspace_row(&b, &wave).await.1,
        path.to_string_lossy(),
        "the refused re-point must not have moved the row"
    );
    assert!(
        trash_entries(&b.workspace_root).is_empty(),
        "the refused re-point must not have touched the filesystem"
    );
}

/// An explicit cwd is honored — the default is a default, not a policy.
///
/// Pinned separately because the natural over-reach of S6 is to force every
/// terminal into the workspace. `POST /api/waves/{id}/terminal-cards` has taken
/// a `cwd` since #13, and nothing in design §更换与冻结 says it stops being
/// respected. The freeze still applies: it is the *row*, not the path it names,
/// that cannot be re-anchored.
#[tokio::test]
async fn an_explicit_terminal_cwd_is_kept_and_still_freezes() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let elsewhere = b.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/waves/{wave}/terminal-cards"),
        Some(json!({"theme": theme(), "cwd": elsewhere.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let cwd: String = sqlx::query_scalar(
        "SELECT t.cwd FROM terminals t JOIN cards c ON c.id = t.card_id WHERE c.wave_id = ?1",
    )
    .bind(&wave)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(PathBuf::from(&cwd), elsewhere);
    assert_ne!(PathBuf::from(&cwd), path);
    assert!(
        workspace_row(&b, &wave).await.2.is_some(),
        "the freeze is a property of persisting the row, not of which path it names"
    );
}

/// Freeze point 3: the wave leaves Draft.
#[tokio::test]
async fn leaving_draft_freezes_the_workspace_and_the_change_is_refused() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    assert_eq!(workspace_row(&b, &wave).await.2, None, "premise: unfrozen");

    let (status, body) = request(
        b.app.clone(),
        "PATCH",
        &format!("/api/waves/{wave}"),
        Some(json!({"lifecycle": "planning"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        workspace_row(&b, &wave).await.2.is_some(),
        "once a wave is past Draft the scheduler, the forge and every worker \
         treat the path as given, so it must be frozen"
    );

    let active_before: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(
        !active_before.is_empty(),
        "premise: the wave has a live spec harness"
    );

    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());

    // The route checks `frozen_at` in the fence transaction, BEFORE the
    // supersede. `wave_workspace_write_tx`'s latch would refuse this write
    // anyway — that is the durable guarantee — but only after the harness has
    // been torn down and restarted. Two layers, and this is what the outer one
    // buys: a wave that was never going to move does not lose its agent.
    let active_after: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(&wave)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        active_after, active_before,
        "a frozen wave's re-point must be refused without disturbing its harness"
    );
}

/// Freeze point 1: the first workspace lease, taken through the production
/// lease preparation + acquisition path rather than a hand-written INSERT.
#[tokio::test]
async fn the_first_workspace_lease_freezes_the_workspace() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    assert_eq!(workspace_row(&b, &wave).await.2, None, "premise: unfrozen");

    let card: String = sqlx::query_scalar(
        "SELECT id FROM cards WHERE wave_id=?1 AND role='spec' ORDER BY created_at, id LIMIT 1",
    )
    .bind(&wave)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();

    let mut tx = b.repo.pool().begin().await.unwrap();
    let target = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &wave,
        &card,
        &b.workspace_root,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(target.join(".git").is_dir());

    // The lease acquisition itself, through the same in-transaction entry
    // point the dispatcher uses. `POST /api/waves/{id}/codex-cards` would
    // reach it too but needs a live codex app-server.
    calm_server::test_seams::acquire_workspace_lease_for_test(
        b.repo.pool(),
        &card,
        &wave,
        "test-owner",
        &target,
    )
    .await
    .unwrap();

    assert!(
        workspace_row(&b, &wave).await.2.is_some(),
        "a lease row stores an absolute path and its worktree is bound to this \
         repository by two absolute pointers a rename would dangle, so the first \
         lease must close the latch"
    );
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
}

/// Freeze point 4 is S4's: a child wave is frozen the moment it is created,
/// because a spec bootstraps a harness on it immediately and there is no
/// window in which it could safely be re-pointed. Asserted here so a future
/// change to the child adapter that un-freezes it turns THIS slice red too.
#[tokio::test]
async fn a_child_wave_is_frozen_at_creation_and_cannot_be_repointed() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (parent, _) = managed_wave(&b, &area, "parent").await;
    let (child, _) = managed_wave(&b, &area, "child").await;
    // The child adapter's own tests cover the creation path; here the point is
    // the *state* it produces, so the row is put into that state directly and
    // the production PATCH route is what gets tested.
    sqlx::query("UPDATE waves SET parent_wave_id=?1, workspace_frozen_at=?2 WHERE id=?3")
        .bind(&parent)
        .bind(1_i64)
        .bind(&child)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, body) = repoint(&b, &child).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert!(trash_entries(&b.workspace_root).is_empty());
}

/// The launchpad must survive its own freeze points.
///
/// Every codex task on the Today panel takes a workspace lease, which is freeze
/// point 1. If that stamped the launchpad, the very next
/// `POST /api/today/launchpad/ensure` would hit the latch in
/// `wave_workspace_write_tx` and 500 — a permanently dead Today panel, and the
/// panel is the one surface a user cannot route around.
///
/// The exclusion lives inside `wave_workspace_freeze_tx` as a SQL clause rather
/// than as an `if` at each freeze point, so this test drives a real freeze point
/// against the real launchpad and then re-runs `ensure`. Measured: removing the
/// clause turns this test red and no other.
#[tokio::test]
async fn a_workspace_lease_never_freezes_the_launchpad() {
    let b = boot().await;
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "launchpad ensure failed: {status} {body}"
    );
    let launchpad: Value = serde_json::from_str(&body).unwrap();
    let wave = launchpad["wave_id"].as_str().unwrap().to_string();
    let spec_card = launchpad["spec_card_id"].as_str().unwrap().to_string();
    assert_eq!(
        workspace_row(&b, &wave).await.2,
        None,
        "premise: the launchpad starts unfrozen (design D9 exception)"
    );

    let mut tx = b.repo.pool().begin().await.unwrap();
    let target = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &wave,
        &spec_card,
        &b.workspace_root,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    calm_server::test_seams::acquire_workspace_lease_for_test(
        b.repo.pool(),
        &spec_card,
        &wave,
        "test-owner",
        &target,
    )
    .await
    .unwrap();

    assert_eq!(
        workspace_row(&b, &wave).await.2,
        None,
        "the launchpad's path is kernel-maintained and must stay re-pointable; \
         a stamp here bricks `today_launchpad_ensure_tx` against the freeze latch"
    );

    // The consequence, stated as behaviour rather than as a column value.
    let (status, body) = request(b.app.clone(), "POST", "/api/today/launchpad/ensure", None).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "the Today panel must still come up after a lease: {status} {body}"
    );

    // …and it is still not user-repointable.
    let (status, body) = repoint(&b, &wave).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

// ---------------------------------------------------------------------------
// The in-memory half of the fence, and the promises made on the refusal paths
// ---------------------------------------------------------------------------

/// The database fence alone is not enough, and this is the test that says so.
///
/// `maybe_issue_turn` reads no durable state, so an observation that was
/// already enqueued before the fence transaction committed still becomes a
/// turn — writing into the directory about to be renamed, and (a process's cwd
/// follows the inode on Linux) into `.trash` afterwards. The route therefore
/// also takes the live handle out of the registry and shuts it down.
///
/// This exists because that half had **no assertion**, and the distinction
/// matters enough to state precisely — an earlier revision of this comment got
/// it wrong and said the code was never executed.
///
/// It was executed. A panic probe planted in the loop fired in **six** tests
/// that never call `install_live_harness`: creating a wave registers a live
/// spec-harness runtime on its own, so the registry is populated naturally and
/// the loop really runs. What was missing is that nothing ever *checked the
/// slot afterwards*, so deleting `harness.get` + `shutdown()` + `remove` turned
/// nothing red — measured. `install_live_harness` is not what makes the code
/// run; it is what gives this test a runtime id it can name and then assert is
/// gone.
///
/// "Never ran" and "ran, unobserved" call for different fixes, and only the
/// second one is true here.
#[tokio::test]
async fn the_fence_also_takes_the_live_harness_out_of_the_registry() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    let runtime_id = install_live_harness(&b, &wave).await;

    let (status, body) = repoint_to(&b, &wave, &target).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        b.harness.get(&runtime_id).is_none(),
        "the fenced runtime {runtime_id} is still live in the registry, so an \
         observation enqueued before the fence committed could still become a \
         turn in the directory that just moved to the trash"
    );
}

/// A refusal must put the harness back — on the OLD path.
///
/// The route's promise is "a refusal leaves nothing behind except a re-opened
/// harness". Tearing the harness down and then returning 409 without the
/// restart would leave the wave alive but its spec agent dead, which is worse
/// than the change the caller was denied.
#[tokio::test]
async fn a_refusal_after_the_fence_reopens_the_harness_on_the_old_path() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, path) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    install_live_harness(&b, &wave).await;
    let starts_before = harness_start_payloads(&b).await.len();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::waves::install_workspace_repoint_race_hook_for_test(
        &wave,
        calm_server::routes::waves::WorkspaceRepointRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = b.app.clone();
    let wave_for_task = wave.clone();
    let target_for_task = target.clone();
    let patch = tokio::spawn(async move {
        request(
            app,
            "PATCH",
            &format!("/api/waves/{wave_for_task}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target_for_task.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await
    });
    entered.notified().await;
    std::fs::write(
        path.join("agent-output.md"),
        b"the turn was still running\n",
    )
    .unwrap();
    release.notify_one();

    let (status, body) = patch.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");

    let payloads = harness_start_payloads(&b).await;
    assert!(
        payloads.len() > starts_before,
        "a refusal that tore the harness down must start it again; no new \
         `spec-harness-start` was submitted"
    );
    let last: Value = serde_json::from_str(payloads.last().unwrap()).unwrap();
    assert_eq!(
        last["cwd"].as_str(),
        Some(path.to_string_lossy().as_ref()),
        "the restart must use the OLD path — nothing moved. payload={last}"
    );
    assert_eq!(
        workspace_row(&b, &wave).await.0,
        "managed",
        "and the row must be untouched"
    );
}

/// A shutdown that fails must not swallow the wave's spec agent.
///
/// By this point the fence transaction has COMMITTED: every runtime is
/// superseded. The shape this replaces removed the registry entry first and
/// then used `?`, so a failing shutdown returned 500 with the runtimes
/// superseded, the entry gone, and the restart skipped — the spec agent was
/// dead with nothing left that would ever start it again.
///
/// The failure is injected (`fail_workspace_repoint_shutdown_for_test`):
/// `SpecHarness::shutdown` only fails on a persistence error that an
/// integration test cannot provoke without dismantling the very runtime row
/// the fence needs. Same deterministic-injection posture S5 used for N16.
#[tokio::test]
async fn a_failed_harness_shutdown_still_completes_the_repoint() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, _) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    install_live_harness(&b, &wave).await;
    let starts_before = harness_start_payloads(&b).await.len();
    calm_server::routes::waves::fail_workspace_repoint_shutdown_for_test(&wave);

    let (status, body) = repoint_to(&b, &wave, &target).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a shutdown failure must not abort a re-point whose fence already \
         committed; body={body}"
    );

    let (kind, path, _) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(path), target);

    let payloads = harness_start_payloads(&b).await;
    assert!(
        payloads.len() > starts_before,
        "the spec agent must have been restarted; leaving it superseded with no \
         restart is the failure mode this test exists for"
    );
}

/// Moving a directory is a human decision.
///
/// This is the only thing standing between an **agent** and pointing a wave at
/// any repository on the box. Issue #985 drew that line for
/// `automation_policy`; a workspace re-point is strictly more destructive.
/// Every other test in this file runs as the user, so without this one the
/// guard has no fixture at all.
///
/// `ai:codex` is not an arbitrary choice, and the reason is worth knowing:
/// `Actor::to_actor_id` maps `"user"` → `User`, `"ai:codex"` → `AiCodex`, and
/// **everything else — including `ai:spec` — to `User`** by a documented
/// defensive default. So `ai:codex` is the only header value that reaches a
/// non-`User` `ActorId` at all, and a test written with `ai:spec` passes
/// vacuously while looking correct (measured: it returned 200 and moved the
/// workspace). That is not a hole this slice opened or should close here —
/// `actor.rs`'s module doc is explicit that the header is a *declared*, not
/// authenticated, identity and "plumbing, not a security boundary", and #985's
/// identical guard has exactly the same reach. What this test pins is that the
/// guard is wired and fires, not that the header cannot be lied about.
#[tokio::test]
async fn a_non_user_actor_may_not_change_a_workspace() {
    let b = boot().await;
    let area = create_area(&b, "c").await;
    let (wave, managed_path) = managed_wave(&b, &area, "w").await;
    let target = user_repo(&b.tmp.path().join("my-project"));
    let target_before = fingerprint(&target);

    // Called directly rather than over HTTP, and that is the finding rather
    // than a shortcut. Measured: driving this through `PATCH` with
    // `X-Calm-Actor: ai:codex` DOES answer 403 — but with
    // `"AiCodex/AiClaude/AiSpec actor has empty card id"`, a different and
    // older guard. `Actor::to_actor_id` maps every header string except
    // `"ai:codex"` to `User`, and `"ai:codex"` carries an empty card id that
    // the outer guard rejects first, so no HTTP request can produce a caller
    // this check would be the first to stop. A test written over the route
    // therefore passes with this guard deleted — it did, and that is why it is
    // written this way.
    // A live harness, so the refusal can be told apart from the OTHER
    // `Forbidden` this call can produce. Deleting the user-only guard does not
    // make the operation succeed — it makes it fail later, inside the write
    // transaction's role gate, AFTER the fence has committed and the harness
    // has been torn down. Both answers are 403, so a test that only checks the
    // status (or even only `matches!(.., Forbidden(_))`) passes either way;
    // measured, twice. What actually differs is *when* it refuses.
    let runtime_id = install_live_harness(&b, &wave).await;

    let wave_row = b.repo.wave_get(&wave).await.unwrap().unwrap();
    let result = calm_server::routes::waves::repoint_wave_workspace_for_test(
        &calm_server::state::RouteState::from_ref(&b.state),
        &calm_server::state::WorkerState::from_ref(&b.state),
        &calm_server::actor::Actor("ai:codex".into()),
        &wave_row,
        &calm_server::model::WaveWorkspacePatch {
            kind: calm_server::model::WaveWorkspaceKind::Attached,
            path: target.to_string_lossy().into_owned(),
            attach_folder: true,
        },
    )
    .await;
    let error = result.expect_err("a non-user actor must be refused");
    assert!(
        matches!(error, calm_server::error::CalmError::Forbidden(_)),
        "expected Forbidden, got {error:?}"
    );
    assert!(
        error.to_string().contains("user-only"),
        "the refusal must be THIS guard's, not the write transaction's role \
         gate answering the same 403 four steps later: {error}"
    );
    assert!(
        b.harness.get(&runtime_id).is_some(),
        "the refusal must land BEFORE the fence: runtime {runtime_id} was torn \
         out of the registry, which means the request got as far as superseding \
         the wave's runtimes before something else refused it"
    );

    let (kind, path, frozen) = workspace_row(&b, &wave).await;
    assert_eq!(kind, "managed");
    assert_eq!(PathBuf::from(path), managed_path);
    assert_eq!(frozen, None);
    assert!(trash_entries(&b.workspace_root).is_empty());
    assert_eq!(
        diff(&target_before, &fingerprint(&target)),
        Vec::<String>::new(),
        "the user's repository must not have been touched"
    );
}
