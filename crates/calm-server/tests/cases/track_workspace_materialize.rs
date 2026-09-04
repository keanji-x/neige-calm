//! Issue #1147 S2 — managed workspace allocation + materialization through
//! `POST /api/tracks`.
//!
//! Design `docs/1147-workspace-design.md` D2/D3/D5 and §5 test 5. The
//! properties this file owns:
//!
//!   * a title-only create (the #1131 shape the new FE sends) allocates a
//!     managed workspace under the configured root and leaves a real git
//!     repository there — without it, every codex task on that track dies in
//!     `git rev-parse --show-toplevel`, which is #1147;
//!   * the same holds for a create that carries a `template_id` and no `cwd`,
//!     which is a distinct branch of the request shape and, since #1300 removed
//!     template seeding, a branch nothing else in the suite drives;
//!   * a materialization failure is a **non-2xx**, not a 201 with a warning in
//!     the log. The latter reproduces #1147 one layer down: the track looks
//!     fine and the first worker dies.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<SqlxRepo>,
    workspace_root: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "ws-materialize".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache),
    )
    .with_workspace_root(workspace_root.clone());
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        area_id: area.id.to_string(),
        repo: sqlx_repo,
        workspace_root,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn theme() -> Value {
    json!({"fg": [255, 255, 255], "bg": [0, 0, 0]})
}

/// The card a hypothetical first worker holds its lease for. Any valid path
/// segment does — the lease target is derived from `<track_id>/<card_id>`, and
/// no card row is read.
const CARD_ID: &str = "card0000000000000000000000000001";

fn head_resolves(path: &std::path::Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn workspace_row(repo: &SqlxRepo, track_id: &str) -> (String, String, Option<i64>) {
    sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id=?1",
    )
    .bind(track_id)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

/// Entry point 1 of 3: `POST /api/tracks` with no `cwd` — the #1131 title-only
/// create the new FE sends.
///
/// The remaining track-create entry points are
/// line 76 enumerates are, by name rather than by ordinal (an ordinal spread
/// across three files is what drifted last time — `today.rs` and
/// `child_track_adapter.rs` both called themselves "the fifth"):
///
///   1. `POST /api/tracks` — this case and the two below it;
///   2. Today/launchpad (`routes::today`), which raw-`INSERT`s and carries its
///      own materialize call;
///   3. child track (`operation::child_track_adapter`), likewise, covered by
///      `child_allocates_and_materializes_its_own_frozen_managed_workspace`.
///
/// #1300 — this enumeration said "of 5" until template seeding was removed.
/// The one that went was `seed_template_track` (design line 76 lists it second,
/// as "workflow/template" — it is named rather than numbered here for the same
/// reason as the four above), and the case that covered it (`seeded_template_tracks_are_materialized`) is
/// gone with the function: a template is a Rust constant now, so creating from
/// one mints exactly the track the caller asked for and there is no second,
/// hidden workspace to materialize. The count is corrected rather than left at
/// 5, because an enumeration that claims more coverage than it has is worse
/// than none.
///
/// What did **not** go away with it is that a `template_id` create is still a
/// create and still needs a workspace —
/// `template_create_without_cwd_allocates_and_materializes_a_managed_workspace`
/// below owns that half, which the deleted case had been covering as a
/// side-effect.
#[tokio::test]
async fn title_only_create_allocates_and_materializes_a_managed_workspace() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"area_id": b.area_id, "title": "research", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap();

    let (kind, path, frozen) = workspace_row(&b.repo, track_id).await;
    assert_eq!(kind, "managed");
    assert_eq!(
        PathBuf::from(&path),
        b.workspace_root.join(&b.area_id).join(track_id),
        "D2 layout is `<root>/<area_id>/<track_id>`, ids only"
    );
    assert!(
        frozen.is_none(),
        "a managed workspace is a *default* and stays re-pointable until work \
         happens (design §2.3 / D4); freezing at create would make S3 vacuous"
    );

    let path = PathBuf::from(path);
    assert!(path.join(".git").is_dir(), "no repository at {path:?}");
    assert!(
        head_resolves(&path),
        "no init commit — `git worktree add` fails with `not a valid object \
         name: 'HEAD'` and the first codex worker never starts"
    );
    // D3 step 4: the exclusion lives in `.git/info/exclude`, and the fresh
    // workspace must look empty to design D4's predicate.
    let exclude = std::fs::read_to_string(path.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l.trim() == ".claude/worktrees/"));
    assert!(!path.join(".gitignore").exists());
}

/// Entry point 1, template branch: a `template_id` create that omits `cwd` gets
/// the same managed workspace, materialized, as a title-only one.
///
/// ## Why this is a separate case and not a parameter of the one above
///
/// #1300 deleted `seeded_template_tracks_are_materialized`, and rightly: its
/// subject — the three hidden system-area template tracks — no longer exists.
/// But that case was two properties in one, and only one of them died with the
/// seeding. The survivor is this: the track the **user** asked for, when it
/// carries a `template_id` and no `cwd`, must still come out of create with a
/// managed directory holding a usable Git repository. Nothing else in the suite
/// held it — the case above never sends a `template_id`, every Rust template
/// case in `track_template_tracks.rs` boots without a workspace root and only
/// reads the report, and the e2e never looks at the workspace at all.
///
/// The escape construction that motivated it, and that this case was
/// mutation-verified against: guard the `materialize_workspace` call in
/// `create_track_structure` with `if track.template_id.is_none()`. Every other
/// test in the repository stays green; the first codex worker on any
/// template-created track then dies in `git rev-parse --show-toplevel`, which is
/// #1147 verbatim.
#[tokio::test]
async fn template_create_without_cwd_allocates_and_materializes_a_managed_workspace() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({
            "area_id": b.area_id,
            "title": "from template",
            "template_id": "small-change",
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        track["template_id"], "small-change",
        "the create must actually have taken the template branch, or this case \
         is a duplicate of the title-only one; body={body}"
    );
    let track_id = track["id"].as_str().unwrap();

    // Exactly one track exists: no hidden template track was minted alongside it
    // (that property's own home is `creating_from_a_template_mints_no_hidden_track`;
    // pinned here too because the loop below would otherwise be satisfiable by a
    // second, differently-materialized row).
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, workspace_kind, workspace_path FROM tracks")
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "expected exactly the requested track; {rows:?}"
    );

    let (kind, path, frozen) = workspace_row(&b.repo, track_id).await;
    assert_eq!(
        kind, "managed",
        "a template create with no `cwd` is managed"
    );
    assert_eq!(
        PathBuf::from(&path),
        b.workspace_root.join(&b.area_id).join(track_id),
        "D2 layout is `<root>/<area_id>/<track_id>`, ids only"
    );
    assert!(
        frozen.is_none(),
        "a managed workspace stays re-pointable until work happens (design \
         §2.3 / D4); a template create is not work"
    );

    let path = PathBuf::from(path);
    assert!(path.join(".git").is_dir(), "no repository at {path:?}");
    assert!(
        head_resolves(&path),
        "no init commit — `git worktree add` fails with `not a valid object \
         name: 'HEAD'` and the first codex worker on this template track never \
         starts"
    );
}

/// An explicit `cwd` is the attached branch: the user pointed at that
/// directory, so the server records it and never creates or `git init`s it.
#[tokio::test]
async fn explicit_cwd_stays_attached_and_is_never_git_inited() {
    let b = boot().await;
    // #1147 S3 — an attached `cwd` must be inside a Git work tree now, so
    // point at a *sub-directory* of one. `git rev-parse` accepts it, and
    // `target/.git` still not existing is exactly what proves the server did
    // not `git init` the directory the user pointed at.
    let user_repo = PathBuf::from(attached_repo_fixture(
        "workspace-materialize-users-own-repo",
    ));
    let target = user_repo.join("users-own-dir");
    std::fs::create_dir_all(&target).unwrap();
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({
            "area_id": b.area_id,
            "title": "attached",
            "cwd": target.to_string_lossy(),
            "attach_folder": true,
            "theme": theme(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();

    let (kind, path, frozen) = workspace_row(&b.repo, track["id"].as_str().unwrap()).await;
    assert_eq!(kind, "attached");
    assert_eq!(PathBuf::from(&path), target);
    assert!(
        frozen.is_some(),
        "attached workspaces are frozen at creation (design D9): `attached → *` \
         is not a legal transition, so an unfrozen attached row is only ever \
         something a buggy PATCH could relocate — i.e. a user repository"
    );
    assert!(
        !target.join(".git").exists(),
        "the server `git init`-ed a directory the user pointed at"
    );
}

/// §5 test 5 — materialization failure must surface as a non-2xx carrying the
/// real error, not a 201 whose first worker then dies with `spawn-failed`.
///
/// The injection is a **plain file** where `<root>/<area_id>` must be a
/// directory, so `mkdir` returns `ENOTDIR`. Deliberately not a read-only
/// parent (`chmod 0555`): CI runs as root, for whom mode bits are advisory,
/// and that injection would pass vacuously.
#[tokio::test]
async fn materialize_failure_fails_the_create() {
    let b = boot().await;
    std::fs::create_dir_all(&b.workspace_root).unwrap();
    std::fs::write(b.workspace_root.join(&b.area_id), "not a directory").unwrap();

    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"area_id": b.area_id, "title": "doomed", "theme": theme()}),
    )
    .await;
    assert!(
        !status.is_success(),
        "materialization failed but the route returned {status}; a 2xx here is \
         #1147 replayed one layer down. body={body}"
    );
    assert!(
        body.contains("materialize workspace"),
        "the response must carry the real error, not a generic one: {body}"
    );

    // ---- known state, deliberately pinned (S2 review ruling ④) ----
    //
    // Materialization runs *after* the track transaction commits, because design
    // D5 requires it outside the tx: the managed path is derived from the track
    // id, which does not exist until the insert. So a failure leaves the track
    // row behind, pointing at a directory that does not exist. S2 does NOT
    // compensate — deleting the track here would have to emit `TrackDeleted` and
    // tear down the two cards minted in the same tx, which is a bigger change
    // than this slice carries.
    //
    // This is asserted rather than tolerated silently: if a later slice adds
    // compensating deletion, this test fails and the author decides
    // deliberately instead of discovering it. Do not "fix" a failure here by
    // loosening the assertion.
    let orphans: Vec<(String, String)> =
        sqlx::query_as("SELECT id, workspace_path FROM tracks WHERE title='doomed'")
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        orphans.len(),
        1,
        "known state: the track row survives a failed materialization"
    );
    assert!(
        !std::path::Path::new(&orphans[0].1).exists(),
        "the orphan row's managed path must not exist on disk — if it does, \
         materialization partially succeeded and this injection is not testing \
         what it claims: {orphans:?}"
    );

    // And the injection really is what broke it: with the obstruction removed
    // the identical request succeeds.
    std::fs::remove_file(b.workspace_root.join(&b.area_id)).unwrap();
    let (status, body) = post(
        b.app,
        "/api/tracks",
        json!({"area_id": b.area_id, "title": "fine", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
}

/// #1318 item 4 — the escape construction that shows `.git` + `HEAD` is not a
/// materialization bar: a workspace that passes both checks can still be
/// unusable by the first worker.
///
/// Construction: rename the materialized workspace's only branch to `neige`
/// (`git branch -m neige`). `.git` is still a directory and `HEAD` still
/// resolves, so **every** assertion the entry-point cases in this file make
/// about a materialized workspace keeps passing. But `refs/heads/neige` now
/// exists as a *file*, so the first worker's
/// `git worktree add -b neige/<track_id>/<card_id>` cannot create
/// `refs/heads/neige/…` under it, and the track is #1147 all over again: every
/// codex task on it dies with nothing but `spawn-failed` visible.
///
/// This case pins both halves — the two old checks passing AND the production
/// lease path failing — so that the "real provisioning" assertions the other
/// cases now carry cannot be weakened back to `.git` + `HEAD` without a red
/// test naming the exact gap. It is deliberately NOT a claim that production
/// should tolerate this state: nothing in the server renames that branch, the
/// construction is adversarial, and the fix belongs in what the tests assert.
#[tokio::test]
async fn a_materialized_workspace_can_pass_the_git_and_head_checks_and_still_fail_the_first_worker()
{
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"area_id": b.area_id, "title": "escape", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap().to_string();
    let (_, path, _) = workspace_row(&b.repo, &track_id).await;
    let path = PathBuf::from(path);

    // The construction.
    let renamed = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["branch", "-m", "neige"])
        .output()
        .unwrap();
    assert!(
        renamed.status.success(),
        "git branch -m neige: {}",
        String::from_utf8_lossy(&renamed.stderr)
    );

    // Half 1: the old bar sees nothing.
    assert!(
        path.join(".git").is_dir(),
        "the construction was supposed to leave the repository in place"
    );
    assert!(
        head_resolves(&path),
        "the construction was supposed to leave HEAD resolvable — if this fails \
         the case no longer demonstrates that `.git` + HEAD is a weak bar"
    );

    // Half 2: the production first-worker path (prepare lease target → commit →
    // provision worktree, the `operation::codex_adapter` order) fails.
    let err = calm_server::test_seams::provision_workspace_lease_for_test(
        b.repo.pool(),
        &track_id,
        CARD_ID,
        &b.workspace_root,
    )
    .await
    .expect_err(
        "a workspace whose only branch is `neige` must not provision a \
         `neige/<track>/<card>` worktree — if this now succeeds, git changed \
         its ref-namespace rules and this case's premise is stale",
    );
    let msg = err.to_string();
    // Matched on the ref names, not on git's prose: the wording of the conflict
    // is localized (`LANG`/`LC_ALL` reach the child), the two ref paths are not.
    assert!(
        msg.contains("git worktree add") && msg.contains("refs/heads/neige'"),
        "the failure must be the `refs/heads/neige` file/directory conflict, \
         not some other error that would make this case pass vacuously: {msg}"
    );
    assert!(
        msg.contains(&format!("refs/heads/neige/{track_id}/{CARD_ID}")),
        "the blocked ref must be this worker's slice branch: {msg}"
    );
}

/// #1147 S2 (red-team B5) — an orphaned track heals when a worker takes its
/// lease, instead of `spawn-failed`-ing forever.
///
/// Create materializes *after* its transaction commits (design D5 requires it
/// outside the tx), so a failure there leaves a committed track row pointing at
/// a directory that does not exist — the known state pinned by
/// `materialize_failure_fails_the_create`. Before this slice nothing would ever
/// retry it, so every `kind: codex` task on that track died in
/// `git rev-parse --show-toplevel` with only `spawn-failed` visible. That is
/// bug #1147, re-created by the slice meant to fix it.
#[tokio::test]
async fn an_unmaterialized_managed_track_heals_when_a_worker_takes_its_lease() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/tracks",
        json!({"area_id": b.area_id, "title": "orphan", "theme": theme()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap().to_string();
    let (_, path, _) = workspace_row(&b.repo, &track_id).await;

    // Reproduce the orphan state: the row exists, the directory does not.
    std::fs::remove_dir_all(&path).unwrap();
    assert!(!std::path::Path::new(&path).exists());

    // The production lease path a codex worker takes.
    let mut tx = b.repo.pool().begin().await.unwrap();
    let repo_root = calm_server::test_seams::prepare_workspace_lease_target_for_test(
        &mut tx,
        &track_id,
        "card0000000000000000000000000001",
        &b.workspace_root,
    )
    .await
    .expect(
        "taking a lease on an un-materialized managed track must repair it, not fail: \
         a permanently `spawn-failed` track is bug #1147 itself",
    );
    tx.commit().await.unwrap();

    assert_eq!(repo_root, std::fs::canonicalize(&path).unwrap());
    assert!(
        head_resolves(std::path::Path::new(&path)),
        "the lease path recreated the directory but not a usable repository"
    );
}
