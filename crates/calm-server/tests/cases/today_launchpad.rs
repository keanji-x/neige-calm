#![cfg(unix)]

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::db::RepoRead;
use calm_server::routes::today::SystemAreaMintCounters;
use calm_server::track_report::TrackReportPayload;
use calm_server::{
    card_role_cache::CardRoleCache,
    db::{Repo, RepoOutOfDomain, sqlite::SqlxRepo},
    event::EventBus,
    plugin_host::{PluginHost, PluginRegistry},
    routes,
    shared_codex_appserver::SharedCodexAppServer,
    state::{AppState, CodexClient, DaemonClient, WriteContext},
    track_area_cache::TrackAreaCache,
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    /// #1253 — this server's own mint counters. Per instance, so a sibling
    /// case in the same binary cannot move them (which a process-global let
    /// happen under a threaded `cargo test`).
    system_area_mint: Arc<SystemAreaMintCounters>,
    /// #1147 S2 — the managed workspace root this boot was pinned to.
    workspace_root: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    boot_with(tmp, repo, "workspaces").await
}

/// #1147 S2 (red-team B1) — a second `AppState` over the **same** database and
/// a **different** workspace root, i.e. exactly what a production upgrade (or a
/// `CALM_WORKSPACE_ROOT` change) looks like to the rows already in
/// `operations`.
async fn boot_with(tmp: TempDir, repo: Arc<SqlxRepo>, root_name: &str) -> Boot {
    boot_with_rendezvous(tmp, repo, root_name, None).await
}

/// #1253 — `boot_with`, plus the option to arm the system-area mint
/// rendezvous. Only the concurrency case passes `Some`.
async fn boot_with_rendezvous(
    tmp: TempDir,
    repo: Arc<SqlxRepo>,
    root_name: &str,
    rendezvous: Option<Arc<tokio::sync::Barrier>>,
) -> Boot {
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    // Seeded from the DB, not left empty: `boot_with` is also used to build a
    // SECOND server over an existing database (the B1 upgrade fixture), and an
    // empty role cache would make `ensure` fail to recognise the existing planner
    // card and try to mint a second one.
    repo.seed_card_role_cache(&roles).await.unwrap();
    repo.seed_track_area_cache(&tracks).await.unwrap();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo_dyn.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let state = AppState::from_parts(
        repo_dyn,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ))
    // #1147 S2 — keep every managed workspace this test mints inside the
    // sandbox, and make the root's location assertable.
    .with_workspace_root(tmp.path().join(root_name));
    let state = match rendezvous {
        Some(barrier) => state.with_system_area_mint_rendezvous(barrier),
        None => state,
    };
    let system_area_mint = Arc::clone(&state.system_area_mint);
    let app = routes::router()
        // `POST /api/today/launchpad/report/reset` extracts a `Principal`, so
        // the session layer has to be present exactly as `main.rs` assembles
        // it. Every other case here is indifferent to it.
        .layer(axum::Extension(calm_server::auth::Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo,
        system_area_mint,
        workspace_root: tmp.path().join(root_name),
        _tmp: tmp,
    }
}

async fn create_area(b: &Boot, name: &str) -> Value {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post("/api/areas")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"name": name, "color": "#abc"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_track(b: &Boot, body: Value) -> Value {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post("/api/tracks")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body={}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

async fn ensure(app: axum::Router) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::post("/api/today/launchpad/ensure")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn first_ensure_mints_launchpad_with_all_cards_and_idle_planner() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    let operation_error: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM operations ORDER BY rowid DESC LIMIT 1")
            .fetch_optional(b.repo.pool())
            .await
            .unwrap()
            .flatten();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body={body}, operation_error={operation_error:?}"
    );
    for key in [
        "track_id",
        "planner_card_id",
        "terminal_card_id",
        "terminal_id",
    ] {
        assert!(
            body[key].as_str().is_some_and(|id| !id.is_empty()),
            "missing {key}: {body}"
        );
    }
    let pool = b.repo.pool();
    let purpose: String = sqlx::query_scalar("SELECT purpose FROM tracks WHERE id=?1")
        .bind(body["track_id"].as_str().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(purpose, "launchpad");
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM cards WHERE track_id=?1 ORDER BY kind")
            .bind(body["track_id"].as_str().unwrap())
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(kinds, ["codex", "terminal", "track-report"]);
    let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(body["planner_card_id"].as_str().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["harness"]["pendingQueue"], serde_json::json!([]));
    assert!(payload["harness"].get("goal").is_none());
}

#[tokio::test]
async fn repeated_ensure_preserves_planner_transcript_and_ids_and_singleton() {
    let b = boot().await;
    let (first_status, first) = ensure(b.app.clone()).await;
    assert_eq!(first_status, StatusCode::CREATED);
    b.repo
        .harness_item_insert(
            "runtime",
            first["planner_card_id"].as_str().unwrap(),
            first["track_id"].as_str().unwrap(),
            "thread",
            Some("turn"),
            Some("item"),
            Some("agent_message"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
    let (second_status, second) = ensure(b.app.clone()).await;
    assert_eq!(second_status, StatusCode::OK, "body={second}");
    assert_eq!(second, first);
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
        .bind(first["planner_card_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(items, 1);
    let launchpads: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE purpose='launchpad'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(launchpads, 1);
}

#[tokio::test]
async fn legacy_today_adoption_resets_planner_transcript_and_preserves_terminal() {
    let b = boot().await;
    let (_, original) = ensure(b.app.clone()).await;
    sqlx::query("UPDATE tracks SET purpose=NULL WHERE id=?1")
        .bind(original["track_id"].as_str().unwrap())
        .execute(b.repo.pool())
        .await
        .unwrap();
    b.repo
        .harness_item_insert(
            "legacy-runtime",
            original["planner_card_id"].as_str().unwrap(),
            original["track_id"].as_str().unwrap(),
            "legacy-thread",
            None,
            Some("legacy-item"),
            Some("agent_message"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=?2 WHERE id=?1")
        .bind(original["planner_card_id"].as_str().unwrap())
        .bind(r#"{"schemaVersion":1,"harness":{"snapshotVersion":9,"pendingQueue":["legacy"]}}"#)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    assert_eq!(adopted, original);
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
        .bind(original["planner_card_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(items, 0);
    let purpose: String = sqlx::query_scalar("SELECT purpose FROM tracks WHERE id=?1")
        .bind(original["track_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(purpose, "launchpad");
    assert_eq!(adopted["terminal_card_id"], original["terminal_card_id"]);
    assert_eq!(adopted["terminal_id"], original["terminal_id"]);
}

/// #1147 S1 (design D9 + §5 test 8). The launchpad is the track-create path
/// that bypasses `track_create_tx` entirely — a raw `INSERT INTO tracks(...)`
/// plus a raw `UPDATE`, with a hand-written `Track` literal and two
/// hand-written SELECT column lists. That combination fails at **runtime**,
/// not at compile time (`query_as` binds columns by name), so this asserts the
/// route still answers and that both of its branches leave the workspace and
/// its `cwd` projection agreeing.
///
/// It also pins the launchpad's documented exception: this track is
/// **never frozen**. The adopt-legacy branch re-points an existing `Today`
/// track at the caller's `cwd`, and `ensure` is idempotent, so a stamp written
/// on the first call would be overwritten on the second — which is precisely
/// the "one-shot, monotonic" property D1 gives `frozen_at`. Leaving it NULL is
/// how re-pointing stays legal without the latch ever being violated.
#[tokio::test]
async fn launchpad_track_carries_an_unfrozen_managed_workspace_on_both_branches() {
    async fn workspace_row(repo: &SqlxRepo, id: &str) -> (String, String, Option<i64>) {
        sqlx::query_as(
            "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id=?1",
        )
        .bind(id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
    }

    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["track_id"].as_str().unwrap().to_string();

    // Branch 1 — fresh INSERT.
    let (kind, path, frozen_at) = workspace_row(&b.repo, &track_id).await;
    // #1147 S2 review ruling — `managed`, not `attached`. The directory is
    // minted by the server, and `attached` means "the user pointed at it, the
    // server never touches it". Labelling a server-created directory
    // `attached` is a row that disagrees with the fact on disk.
    assert_eq!(kind, "managed");
    assert!(
        std::path::Path::new(&path).starts_with(&b.workspace_root),
        "the launchpad must live under the workspace root so \
         `managed ⇒ under <workspace-root>` holds with no exceptions: {path}"
    );
    assert_eq!(
        frozen_at, None,
        "the launchpad track must stay unfrozen so the adopt branch can \
         re-point it without re-stamping (design D1 monotonicity)"
    );

    // The read path must survive the new columns (this is where a stale
    // SELECT column list detonates).
    let response = b
        .app
        .clone()
        .oneshot(
            Request::get(format!("/api/tracks/{track_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let dto: Value = serde_json::from_slice(&bytes).unwrap();
    let track = dto.get("track").unwrap_or(&dto);
    assert_eq!(track["workspace"]["kind"], "managed", "dto={dto}");
    assert_eq!(track["workspace"]["path"], track["cwd"], "dto={dto}");
    assert!(track["workspace"]["frozen_at"].is_null(), "dto={dto}");

    // Branch 2 — legacy adoption. Scramble the row so the assertion cannot
    // pass on leftovers from branch 1: the adoption branch must rewrite both
    // columns through the single writer.
    sqlx::query(
        // #1147 S3 — the stamp is scrambled to NULL, not to 99. S2 flipped it
        // to 99 to keep the assertion non-vacuous, but S3's freeze latch makes
        // a frozen launchpad row un-repointable, so a 99 here would fake a
        // state nothing can produce (`track_workspace_freeze_tx` excludes the
        // system area precisely so this row never gets a stamp) and would turn
        // the adopt branch into a 409. `kind` and `path` still carry the
        // scramble, so the rewrite assertions below still bite.
        "UPDATE tracks SET purpose=NULL, workspace_path='/also-scrambled', \
         workspace_kind='attached', workspace_frozen_at=NULL WHERE id=?1",
    )
    .bind(&track_id)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    let (kind, path, frozen_at) = workspace_row(&b.repo, &track_id).await;
    assert_eq!(kind, "managed", "adoption re-declares the kind");
    assert_ne!(
        path, "/also-scrambled",
        "adoption actually rewrote the path"
    );
    assert!(std::path::Path::new(&path).starts_with(&b.workspace_root));
    assert_eq!(
        frozen_at, None,
        "adoption re-points, and re-pointing must never stamp frozen_at"
    );
}

/// #1147 S2 review ruling ① — the invariant that pays for making the launchpad
/// `Managed`: **`kind = Managed` ⇒ the path is under `<workspace-root>`, with
/// no exceptions.**
///
/// S5's recycle path asserts this prefix before it removes anything. Stated
/// over the whole table rather than per known track, so any future create path
/// that mints a managed workspace somewhere else shows up here whether or not
/// somebody remembered to write a test for it — and so S5 never needs a
/// launchpad carve-out.
#[tokio::test]
async fn every_managed_track_lives_under_the_workspace_root() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // A user area with both shapes of track, minted through the production
    // route: a managed one (title-only) and an attached one (explicit cwd), so
    // the property below is neither empty nor all-launchpad.
    let area = create_area(&b, "Atlas").await;
    let attached_dir = attached_repo_fixture("today-launchpad-under-root");
    create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "managed track",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "attached track",
            "cwd": attached_dir.clone(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;

    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, workspace_kind, workspace_path, purpose FROM tracks ORDER BY created_at, id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    let managed: Vec<_> = rows.iter().filter(|r| r.1 == "managed").collect();
    let attached: Vec<_> = rows.iter().filter(|r| r.1 == "attached").collect();
    assert!(
        managed.len() >= 2 && managed.iter().any(|r| r.3.as_deref() == Some("launchpad")),
        "the managed set must include the launchpad AND at least one ordinary \
         track, or this property is vacuous: {rows:?}"
    );
    assert!(
        !attached.is_empty(),
        "an attached track must exist too, otherwise the negative check below \
         could be passing because every row happens to be managed: {rows:?}"
    );
    for (id, _, path, purpose) in managed {
        assert!(
            std::path::Path::new(path).starts_with(&b.workspace_root),
            "managed track {id} (purpose={purpose:?}) lives at {path}, outside \
             the workspace root {}. S5 removes managed directories: a managed \
             row pointing outside the root is either an un-recyclable orphan \
             or, worse, something S5 would delete outside its own tree.",
            b.workspace_root.display()
        );
    }
    // The attached one must NOT be under the root — otherwise the property
    // above would hold trivially, for the wrong reason.
    for (id, _, path, _) in attached {
        assert!(
            !std::path::Path::new(path).starts_with(&b.workspace_root),
            "attached track {id} at {path} is inside the managed root"
        );
    }
}

/// `(track id, workspace_kind, workspace_frozen_at, purpose, area kind)` —
/// named so `no_attached_track_is_ever_unfrozen`'s query stays inside clippy's
/// `type_complexity` budget.
type WorkspaceFreezeRow = (String, String, Option<i64>, Option<String>, String);

/// #1147 — the bound on the "may still be re-pointed" state (design D9, r3.2
/// amendment; narrowed by the S2 review).
///
/// S1 could say "no track outside the launchpad is unfrozen" only because every
/// track it could mint was `attached`. S2 mints `managed` workspaces, and
/// **unfrozen managed is the intended steady state** — it is exactly what
/// makes S3's "re-point before any work has happened" reachable. So the
/// property is not about unfrozen rows in general.
///
/// What must stay impossible is an unfrozen **attached** row. `attached` is a
/// repository the user pointed at; `frozen_at IS NULL` means "a PATCH may
/// relocate this". Together they are a real user repository sitting in the
/// state a PATCH branch that forgot to check `kind` would move (design D9).
/// Since the S2 review made the launchpad `managed`, that combination now has
/// **no exceptions at all** — the set must be empty.
///
/// An empty-set property is worthless without proof that the population is
/// non-empty, so this asserts both halves of the population exist first:
/// attached rows (which must all be frozen) and unfrozen rows (which must all
/// be managed).
#[tokio::test]
async fn no_attached_track_is_ever_unfrozen() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // Both shapes in a *user* area, minted through the production route —
    // otherwise the property is only tested against the row that motivated it.
    let area = create_area(&b, "Atlas").await;
    let attached_dir = attached_repo_fixture("today-launchpad-never-unfrozen");
    create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "user track",
            "cwd": attached_dir.clone(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "managed track",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;

    let rows: Vec<WorkspaceFreezeRow> = sqlx::query_as(
        "SELECT w.id, w.workspace_kind, w.workspace_frozen_at, w.purpose, c.kind \
         FROM tracks w JOIN areas c ON c.id = w.area_id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();

    // Non-vacuity, both directions.
    assert!(
        rows.iter().any(|r| r.1 == "attached"),
        "no attached track exists, so the property below is vacuous: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.2.is_none()),
        "no unfrozen track exists, so the property below is vacuous: {rows:?}"
    );

    let unfrozen_attached: Vec<_> = rows
        .iter()
        .filter(|r| r.1 == "attached" && r.2.is_none())
        .collect();
    assert!(
        unfrozen_attached.is_empty(),
        "`attached + frozen_at IS NULL` is a user repository in the state a \
         kind-blind PATCH would relocate (design D9). This combination has no \
         legal instance: {unfrozen_attached:?}"
    );

    // And every surviving unfrozen row is managed — the launchpad plus
    // whatever the managed default minted.
    for (id, kind, frozen_at, purpose, area_kind) in rows.iter().filter(|r| r.2.is_none()) {
        assert_eq!(
            kind, "managed",
            "track {id} (purpose={purpose:?}, area={area_kind}, frozen_at={frozen_at:?})"
        );
    }
    assert!(
        rows.iter()
            .any(|r| r.3.as_deref() == Some("launchpad") && r.2.is_none() && r.4 == "system"),
        "the kernel-owned launchpad must still be the unfrozen system-area \
         track design D9 carved out: {rows:?}"
    );
}

/// #1147 S2 (design D3) — the launchpad is the fifth track-create entry point
/// and bypasses `create_track_structure` entirely (raw `INSERT INTO tracks`).
/// Without its own materialize call every codex task on the Today panel keeps
/// dying with `spawn-failed`, which is the defect #1147 was opened on.
#[tokio::test]
async fn launchpad_workspace_is_materialized() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (path, kind, frozen_at, area_id): (String, String, Option<i64>, String) = sqlx::query_as(
        "SELECT workspace_path, workspace_kind, workspace_frozen_at, area_id FROM tracks \
         WHERE purpose='launchpad'",
    )
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    // #1147 S2 review ruling ① — a managed workspace like any other, at
    // `<root>/<system_area_id>/<track_id>`, and still never frozen (design D9).
    assert_eq!(kind, "managed");
    assert_eq!(
        frozen_at, None,
        "the launchpad stays re-pointable; that D9 exception survives the move \
         to a managed root"
    );
    assert_eq!(
        std::path::PathBuf::from(&path),
        b.workspace_root
            .join(area_id)
            .join(body["track_id"].as_str().unwrap())
    );
    let path = std::path::Path::new(&path);
    assert!(path.join(".git").is_dir(), "no repository at {path:?}");
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap()
            .status
            .success(),
        "the launchpad workspace has no init commit; `git worktree add` fails \
         and every Today codex task stays `spawn-failed`"
    );
    let exclude = std::fs::read_to_string(path.join(".git/info/exclude")).unwrap();
    assert!(
        exclude
            .lines()
            .any(|line| line.trim() == ".claude/worktrees/")
    );
    assert!(!path.join(".gitignore").exists());

    // `ensure` is idempotent, and so is materialize.
    let (status, body) = ensure(b.app).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
}

/// #1147 S2 (red-team B1, blocking) — a workspace re-point must not wedge the
/// Today panel on a stale idempotency key.
///
/// The `planner-harness-start` payload carries `cwd`, and the operation runtime
/// refuses a key already used with a *different* payload hash. A pre-S2
/// database already holds `today-launchpad:<card>:reuse` rows hashed against
/// the old path; once the upgrade re-points the workspace, every later `ensure`
/// would resubmit that key with the new cwd and 409 — permanently, because
/// nothing ever deletes rows from `operations`.
///
/// Reproduced through production paths only: run `ensure` to steady state
/// under one root (minting a real `:reuse` operation row), then rebuild the
/// server over the same database with a different root — which is what both a
/// pre-S2 upgrade and a `CALM_WORKSPACE_ROOT` change look like from the DB's
/// point of view. The two `ensure` calls afterwards are the first and second
/// request after deploy; both must succeed.
#[tokio::test]
async fn repointing_the_workspace_does_not_wedge_ensure_on_a_stale_idempotency_key() {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let before = boot_with(TempDir::new().unwrap(), repo.clone(), "workspaces-old").await;
    // Hold the old root's tempdir for the lifetime of this test.
    let _old_tmp = before._tmp;

    let (status, body) = ensure(before.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    // Steady state: this is the call that mints the `:reuse` operation row
    // hashed against the OLD workspace path.
    let (status, body) = ensure(before.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let old_path: String =
        sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE purpose='launchpad'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let reuse_keys: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations WHERE idempotency_key LIKE 'today-launchpad:%:reuse%'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(
        reuse_keys > 0,
        "the fixture must have minted a `:reuse` operation row, otherwise there \
         is no stale key to collide with and this test proves nothing"
    );

    // --- the upgrade ---
    let after = boot_with(tmp, repo.clone(), "workspaces-new").await;

    // 200, not 201: the launchpad was neither minted nor adopted, only
    // re-pointed. What matters is that it is a success at all.
    let (status, body) = ensure(after.app.clone()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "first ensure after the re-point must succeed; body={body}"
    );
    let new_path: String =
        sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE purpose='launchpad'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_ne!(
        new_path, old_path,
        "the fixture must actually re-point the workspace, otherwise the stale \
         key never collides and this test proves nothing"
    );

    // The one that used to 409: `:reuse` again, now with the new cwd.
    let (status, body) = ensure(after.app.clone()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "second ensure after the re-point 409'd on a stale idempotency key — \
         the Today panel is wedged with no self-healing path; body={body}"
    );
    // And it stays healed.
    let (status, body) = ensure(after.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
}

/// #1147 N3 (blocking) — a re-point that fails midway must still re-anchor the
/// planner harness on the next `ensure`.
///
/// The intent used to be an in-memory comparison inside the transaction, true
/// for exactly one request. Materialization runs *after* that transaction
/// commits, so a failure there (500) threw the intent away: the next `ensure`
/// saw `stored == desired`, called it steady state, and started the harness
/// with `force_new_thread: false` — pinning the planner agent's codex thread to
/// the OLD cwd forever while every worker used the new one.
///
/// Sequence below is exactly that: upgrade, obstruct materialization so the
/// first post-upgrade `ensure` 500s, clear the obstruction, and require that
/// the harness is still re-anchored (a `:repoint` operation exists).
#[tokio::test]
async fn a_failed_materialize_during_a_repoint_still_re_anchors_the_harness() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let before = boot_with(TempDir::new().unwrap(), repo.clone(), "workspaces-old").await;
    let _old_tmp = before._tmp;

    let (status, body) = ensure(before.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let (status, body) = ensure(before.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // --- the upgrade, with materialization obstructed ---
    let tmp = TempDir::new().unwrap();
    let after = boot_with(tmp, repo.clone(), "workspaces-new").await;
    let area_id: String =
        sqlx::query_scalar("SELECT area_id FROM tracks WHERE purpose='launchpad'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    std::fs::create_dir_all(&after.workspace_root).unwrap();
    // A plain file where `<root>/<area_id>` must be a directory: `mkdir` gets
    // ENOTDIR. (Not a read-only parent — CI runs as root, for whom mode bits
    // are advisory, and that injection would pass vacuously.)
    std::fs::write(after.workspace_root.join(&area_id), "not a directory").unwrap();

    let (status, _) = ensure(after.app.clone()).await;
    assert!(
        !status.is_success(),
        "materialization was obstructed, so this ensure must fail"
    );

    std::fs::remove_file(after.workspace_root.join(&area_id)).unwrap();
    let (status, body) = ensure(after.app.clone()).await;
    assert!(status.is_success(), "body={body}");

    // The load-bearing assertion: the harness was re-anchored at the new path,
    // not resumed at the old one.
    let repoints: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations WHERE idempotency_key LIKE 'today-launchpad:%:repoint:%'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(
        repoints > 0,
        "no `:repoint` operation exists: the re-point intent was lost when the \
         first attempt failed, so the planner harness resumed its thread in the \
         OLD workspace while every worker uses the new one"
    );

    // And it settles: once started at this path, later ensures are plain reuse.
    let (status, body) = ensure(after.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let reuse_at_new_path: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations WHERE idempotency_key LIKE 'today-launchpad:%:reuse:%'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(reuse_at_new_path > 0, "steady state never resumed");
}

/// `(track id, workspace_kind, workspace_path)`.
type WorkspaceRow = (String, String, String);

/// Every track row, whatever minted it.
async fn all_workspace_rows(repo: &SqlxRepo) -> Vec<WorkspaceRow> {
    sqlx::query_as("SELECT id, workspace_kind, workspace_path FROM tracks ORDER BY created_at, id")
        .fetch_all(repo.pool())
        .await
        .unwrap()
}

/// Managed paths held by more than one track — the violation set of the property
/// below. Computed in Rust over whole-table rows rather than as a `GROUP BY`,
/// so the failure message can name the tracks.
fn shared_managed_paths(rows: &[WorkspaceRow]) -> Vec<(String, Vec<String>)> {
    let mut by_path: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for (id, kind, path) in rows.iter().filter(|r| r.1 == "managed") {
        let _ = kind;
        by_path.entry(path.as_str()).or_default().push(id.clone());
    }
    by_path
        .into_iter()
        .filter(|(_, tracks)| tracks.len() > 1)
        .map(|(path, tracks)| (path.to_string(), tracks))
        .collect()
}

/// Same grouping, restricted to `attached` — used only to prove the property
/// above is scoped on purpose and would notice if the scope were wrong.
fn shared_attached_paths(rows: &[WorkspaceRow]) -> Vec<(String, Vec<String>)> {
    let mut by_path: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for (id, _, path) in rows.iter().filter(|r| r.1 == "attached") {
        by_path.entry(path.as_str()).or_default().push(id.clone());
    }
    by_path
        .into_iter()
        .filter(|(_, tracks)| tracks.len() > 1)
        .map(|(path, tracks)| (path.to_string(), tracks))
        .collect()
}

/// Drive the production child-track creation path (`ChildTrackAdapter`, the
/// fifth track-create entry point) against this boot's database and workspace
/// root. The parent task row is seeded directly because the adapter only reads
/// the frozen task fields from it; everything that decides a workspace runs in
/// production code.
async fn create_child_track(b: &Boot, parent_track_id: &str) -> String {
    use calm_server::operation::child_track_adapter::{
        ChildTrackAdapter, ChildTrackOperationPayload,
    };
    use calm_server::operation::{Operation, Phase, ProviderAdapter};

    let task_id = format!("{parent_track_id}:child");
    let now = calm_server::model::now_ms();
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,acceptance_criteria,\
         depends_on_json,priority,status,declared_by,spawn,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,'child','codex','child goal','{}','done','[]',0,'dispatched',\
         'spec','sub-wave',?3,?3)",
    )
    .bind(&task_id)
    .bind(parent_track_id)
    .bind(now)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let payload = serde_json::to_value(ChildTrackOperationPayload {
        task_id: task_id.clone(),
        parent_track_id: parent_track_id.into(),
        goal: "child goal".into(),
        acceptance: Some("done".into()),
        context: serde_json::json!({}),
        cwd: None,
    })
    .unwrap();
    let operation = Operation {
        id: "op-child".into(),
        operation_key: task_id.clone(),
        kind: "child-track".into(),
        idempotency_key: Some(task_id.clone()),
        payload_hash: "test-hash".into(),
        target_type: "unknown".into(),
        target_id: None,
        target: serde_json::json!({"type": "unknown", "id": null}),
        payload: payload.clone(),
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    };
    let adapter = ChildTrackAdapter::new(
        b.repo.card_role_cache().clone(),
        b.repo.track_area_cache().clone(),
        b.workspace_root.clone(),
    );
    let mut tx = b.repo.pool().begin().await.unwrap();
    let output = adapter
        .prepare_tx(&mut tx, &payload, &operation)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    output.data["child_track_id"].as_str().unwrap().to_string()
}

/// #1147 S4 (design D7 as amended) — **no two tracks share a `managed`
/// workspace path**, over the WHOLE table.
///
/// This is the invariant that pays for S5 being allowed to `remove_dir_all` a
/// managed directory: if two rows named one managed directory, deleting either
/// track would destroy the other's repository. It is stated here, beside
/// `every_managed_track_lives_under_the_workspace_root`, for the same reason
/// that one is: a future create path that shares a managed directory shows up
/// whether or not anybody remembered to write a test for it.
///
/// The rows come from the real entry points — launchpad `ensure`, `POST
/// /api/tracks` (managed and attached), and the child-track adapter for both a
/// managed and an attached parent — not from hand-written rows.
///
/// Scoped to `managed` deliberately, with a two-sided guard so the scope cannot
/// silently become vacuous:
///
/// * the managed set must be non-empty and contain more than the launchpad,
///   otherwise "no duplicates" holds because there is nothing to duplicate;
/// * an attached path that IS shared must exist, and must NOT be reported.
///   Attached sharing is legal and pre-existing in production (two tracks on one
///   checkout), so an unscoped version of this property would call a
///   long-standing correct state a violation.
#[tokio::test]
async fn no_two_tracks_share_a_managed_workspace_path() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let area = create_area(&b, "Atlas").await;
    let attached_dir = attached_repo_fixture("today-launchpad-shared-path");
    let managed_parent = create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "managed parent",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    let attached_parent = create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "attached parent",
            "cwd": attached_dir.clone(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;

    // One child under each shape of parent. The attached one is what makes the
    // "attached sharing exists and is not reported" guard below real rather
    // than hypothetical: it produces a genuine shared attached path through
    // production code.
    let managed_child = create_child_track(&b, managed_parent["id"].as_str().unwrap()).await;
    let attached_child = create_child_track(&b, attached_parent["id"].as_str().unwrap()).await;

    let rows = all_workspace_rows(&b.repo).await;
    let managed: Vec<_> = rows.iter().filter(|r| r.1 == "managed").collect();
    assert!(
        managed.len() >= 3,
        "the managed set must hold the launchpad, an ordinary track and a \
         child, or 'no duplicates' is vacuous: {rows:?}"
    );
    assert!(
        managed.iter().any(|r| r.0 == managed_child),
        "the managed child must be in the table: {rows:?}"
    );

    let violations = shared_managed_paths(&rows);
    assert!(
        violations.is_empty(),
        "two tracks share a managed workspace path. S5 recycles managed \
         directories, so deleting either track destroys the other's \
         repository: {violations:?}"
    );

    // The other side of the scope: attached sharing exists here, on purpose,
    // and is NOT a violation.
    let attached_sharing = shared_attached_paths(&rows);
    assert_eq!(
        attached_sharing.len(),
        1,
        "the attached child must share its parent's path, otherwise this test \
         proves nothing about the scoping: {rows:?}"
    );
    let (shared_path, sharers) = &attached_sharing[0];
    assert_eq!(shared_path, &attached_dir);
    assert!(
        sharers.contains(&attached_child) && sharers.len() == 2,
        "expected exactly the attached parent and its child on {shared_path}: \
         {sharers:?}"
    );
}

/// Single-violation fixture for the property above: one managed row moved onto
/// another managed row's path — written through the single workspace writer,
/// which is exactly the surface S3's workspace PATCH will call.
///
/// Without this, "no violations" could be passing because nothing in the tree
/// can produce one today.
#[tokio::test]
async fn shared_managed_path_is_reported_as_a_violation() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let area = create_area(&b, "Atlas").await;
    let first = create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "first",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    let second = create_track(
        &b,
        serde_json::json!({
            "area_id": area["id"],
            "title": "second",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    assert!(shared_managed_paths(&all_workspace_rows(&b.repo).await).is_empty());

    let first_id = first["id"].as_str().unwrap().to_string();
    let second_id = second["id"].as_str().unwrap().to_string();
    let first_path: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id=?1")
        .bind(&first_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let mut tx = b.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::track_workspace_write_tx(
        &mut tx,
        &second_id,
        &calm_server::model::TrackWorkspace {
            kind: calm_server::model::TrackWorkspaceKind::Managed,
            path: first_path.clone(),
            frozen_at: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let violations = shared_managed_paths(&all_workspace_rows(&b.repo).await);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert_eq!(violations[0].0, first_path);
    assert!(
        violations[0].1.contains(&first_id) && violations[0].1.contains(&second_id),
        "the report must name both tracks: {violations:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// #1253 PR1 — the read-only resolve (`GET /api/today/launchpad`) and the
// `is_unique_constraint` fix.
// ─────────────────────────────────────────────────────────────────────────

async fn resolve(app: axum::Router) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::get("/api/today/launchpad")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn count(b: &Boot, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(b.repo.pool())
        .await
        .unwrap()
}

async fn report_card_id(b: &Boot, track_id: &str) -> String {
    sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND kind='track-report'")
        .bind(track_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap()
}

async fn write_report_payload(b: &Boot, card_id: &str, payload: &Value) {
    sqlx::query("UPDATE cards SET payload=?2 WHERE id=?1")
        .bind(card_id)
        .bind(payload.to_string())
        .execute(b.repo.pool())
        .await
        .unwrap();
}

/// INV-TODAYDOC-001 — the page-load path is a *read*. Before any launchpad
/// exists it answers `200 null` and leaves the database and the workspace root
/// exactly as it found them: no area, no track, no card, and above all no
/// `planner-harness-start` operation, because submitting one is what would make
/// Today's first paint depend on codex being up.
///
/// **`200 null`, not `404`, and that is a contract this pins rather than an
/// incidental detail.** A fresh workspace having no launchpad is the ordinary
/// state of the landing route; a 404 put a browser console error on every such
/// session and broke two Playwright specs that assert none. Routine absence is
/// data. (The anomalous absence — a launchpad with no report card — is still a
/// 404; see `resolve_404s_when_the_launchpad_has_no_report_card`.)
#[tokio::test]
async fn resolve_is_a_pure_read_and_answers_null_before_the_launchpad_exists() {
    let b = boot().await;
    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body,
        Value::Null,
        "routine absence must be a null body, not an error status: {body}"
    );
    assert_eq!(count(&b, "SELECT COUNT(*) FROM tracks").await, 0);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM areas").await, 0);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM operations").await, 0);
    assert!(
        !b.workspace_root.exists(),
        "the resolve must not materialize a workspace: {:?}",
        b.workspace_root
    );
}

/// INV-TODAYDOC-003 (positive) — a launchpad whose report is the canonical
/// freshly-minted one reports `report_has_noninitial_content: false`, so the
/// page renders an empty state rather than the four empty H1s the initial body
/// carries. And INV-TODAYDOC-001 again: resolving submits no new operation.
#[tokio::test]
async fn resolve_reports_a_freshly_minted_report_as_unwritten() {
    let b = boot().await;
    let (status, ensured) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={ensured}");
    let operations_after_ensure = count(&b, "SELECT COUNT(*) FROM operations").await;

    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["track_id"], ensured["track_id"]);
    assert_eq!(
        body["report_has_noninitial_content"],
        Value::Bool(false),
        "the canonical initial report has not been written by anyone: {body}"
    );
    // The narrow DTO is exactly two fields — `report_card_id` deliberately is
    // not one of them (§5.1), and neither is anything `ensure` returns.
    let object = body.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["report_has_noninitial_content", "track_id"]);
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM operations").await,
        operations_after_ensure,
        "the resolve must not submit an operation"
    );
}

/// INV-TODAYDOC-003, the **reverse direction** the design calls out by name.
///
/// `report_startup_read_required` compares `summary` + `body` and deliberately
/// ignores `doc_rev` and `blocks`. A canonical initial report that CRDT has
/// already materialised — non-zero `docRev`, a populated `blocks` array — is
/// still an unwritten report, and must still yield the empty state. An earlier
/// revision of this design got exactly this cell wrong.
#[tokio::test]
async fn a_crdt_materialized_canonical_report_still_reads_as_unwritten() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let card = report_card_id(&b, ensured["track_id"].as_str().unwrap()).await;
    let mut payload = serde_json::to_value(TrackReportPayload::initial()).unwrap();
    payload["docRev"] = serde_json::json!(7);
    payload["blocks"] = serde_json::json!([
        {"id": "b1", "kind": "prose", "rev": 3, "payload": {"markdown": "# 概要\n"}}
    ]);
    write_report_payload(&b, &card, &payload).await;

    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["report_has_noninitial_content"],
        Value::Bool(false),
        "docRev/blocks must not flip the predicate: {body}"
    );
}

/// The other half of INV-TODAYDOC-003: once `summary` or `body` differs from
/// the canonical initial, the document is what the page shows.
#[tokio::test]
async fn a_written_report_reads_as_having_content() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let card = report_card_id(&b, ensured["track_id"].as_str().unwrap()).await;
    let mut payload = serde_json::to_value(TrackReportPayload::initial()).unwrap();
    payload["body"] = serde_json::json!("# 概要\n\n今天合了两个 PR。\n");
    write_report_payload(&b, &card, &payload).await;

    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["report_has_noninitial_content"], Value::Bool(true));

    // A summary alone is enough, with the body left canonical.
    let mut summary_only = serde_json::to_value(TrackReportPayload::initial()).unwrap();
    summary_only["summary"] = serde_json::json!("两个 PR");
    write_report_payload(&b, &card, &summary_only).await;
    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["report_has_noninitial_content"], Value::Bool(true));
}

/// §5.1 — a launchpad track with no report card is a 404.
///
/// This state is **not** reachable in production (track and report card are
/// created in one transaction), which is why the endpoint gets no repair path:
/// the test pins the fail-closed answer, not a supported state.
#[tokio::test]
async fn resolve_404s_when_the_launchpad_has_no_report_card() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let card = report_card_id(&b, ensured["track_id"].as_str().unwrap()).await;
    sqlx::query("DELETE FROM cards WHERE id=?1")
        .bind(&card)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
}

/// A payload this build cannot parse is not the canonical initial payload, so
/// it reads as content and the document is shown. The alternative — reading it
/// as "empty" — would let one bad row silently hide a real report.
#[tokio::test]
async fn an_unreadable_report_payload_reads_as_having_content() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let card = report_card_id(&b, ensured["track_id"].as_str().unwrap()).await;
    write_report_payload(&b, &card, &serde_json::json!({"schemaVersion": 3})).await;

    let (status, body) = resolve(b.app.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["report_has_noninitial_content"], Value::Bool(true));
}

/// The `is_unique_constraint` fix, exercised on the route path rather than on
/// the helper.
///
/// Both `ensure` calls read `area_get_system()` — `None` for both — before
/// either opens a write transaction, so both try to mint the system area and
/// the loser hits `idx_areas_one_system`. SQLite words that as
/// `UNIQUE constraint failed: areas.kind`, so while this module matched on the
/// *index* name the retry arm was dead code and the loser returned 500. The
/// assertion is therefore about the losing request's status, not about the
/// singleton (which the index enforces either way and which was already true
/// before the fix).
///
/// `is_unique_constraint_for_test` deliberately is not used here: feeding a
/// hand-built `CalmError` to the helper tests the helper, and the helper was
/// never the defect — the call sites were.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_ensure_retries_the_system_area_race() {
    // ── This case asserts the MECHANISM, not just the outcome. ──
    //
    // Its previous form checked only that both requests succeeded and that the
    // singletons held. Every one of those is *also* true when the race never
    // happens: then only one request mints, the retry arm is never needed, and
    // a broken arm is never touched. Nothing orders the two requests — A can
    // reuse a warm pool connection and finish the whole `ensure` while B is
    // still establishing one, after which B reads `Some` — so "green" on its
    // own carried no information about whether anything had been exercised.
    //
    // Two halves, and both are needed.
    //
    // The RENDEZVOUS *creates* the race: armed on this server only, it parks
    // each request after its `area_get_system()` has returned `None` and before
    // it opens a write transaction, so the second request provably cannot read
    // the first one's committed row. Without it the case merely hoped —
    // `tokio::join!` imposes no order, and on a CI runner request A finished
    // the whole mint before B read, so the case went red reporting, correctly,
    // that the race had not happened. Red-flaky is worse than the vacuous
    // version it replaced.
    //
    // `attempts == 2` then *proves* the rendezvous did its job, and keeps the
    // status assertion below from being vacuous. A rendezvous with no counter
    // would be an unchecked assumption; a counter with no rendezvous is what
    // CI falsified.
    //
    // `sqlite::memory:` like every sibling case here, measured on this exact
    // two-request shape: 8/8 green unmutated with `attempts == 2` holding, and
    // 8/8 red with the retry arm broken. An earlier revision used an on-disk
    // WAL database on the theory that in-memory sqlite suppresses the race;
    // that reading came from a different shape (six barrier-released requests)
    // and did not survive being re-measured against this one, so the special
    // case is deleted rather than re-justified.
    let gate = Arc::new(tokio::sync::Barrier::new(2));
    let b = boot_with_rendezvous(
        TempDir::new().unwrap(),
        Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap()),
        "workspaces",
        Some(Arc::clone(&gate)),
    )
    .await;
    // The second request is deliberately SKEWED, and that is what makes this a
    // gate rather than a coin flip.
    //
    // Without the rendezvous, a 250ms head start lets the first request finish
    // its entire mint before the second one reads, so the second reads `Some`,
    // never mints, and `attempts` is 1 — measured, this reproduces the CI
    // failure exactly ("the race did not happen: 1 of the 2 requests found no
    // system area"). With the rendezvous the first request is parked before it
    // can write, so the skew cannot suppress the race and the case passes.
    //
    // Keeping the skew permanently means removing the rendezvous fails here
    // deterministically, on any machine, instead of waiting for a scheduler
    // unlucky enough to expose it. Our box was green 20/20 on the version CI
    // then falsified; this is the difference.
    //
    // Bounded, because the failure mode of a rendezvous is a hang: if only one
    // request ever reached it, `wait()` would park forever and CI would report
    // a timeout instead of a fact. 30s is far above the ~0.5s this takes.
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let a = b.app.clone();
        let c = b.app.clone();
        tokio::join!(ensure(a), async move {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            ensure(c).await
        })
    })
    .await
    .expect("both requests must reach the mint rendezvous; a timeout here means only one did");
    for (status, body) in [&first, &second] {
        assert!(
            status.is_success(),
            "a concurrent first ensure must retry the system-area race, not 500: {status} {body}"
        );
    }
    // Absolute, not a delta: these counters belong to THIS server instance and
    // nothing else touched it.
    let attempts = b.system_area_mint.attempts.load(Ordering::Relaxed);
    let retries = b.system_area_mint.retries.load(Ordering::Relaxed);
    // Both requests read "no system area" before either wrote — i.e. the race
    // this case exists for actually occurred. Without this, every assertion
    // below is satisfied by a run in which the second request simply read the
    // first one's committed row and no retry was ever needed.
    assert_eq!(
        attempts, 2,
        "the race did not happen: {attempts} of the 2 requests found no system \
         area, so nothing exercised the retry arm and the assertions below \
         prove nothing"
    );
    // …and exactly one of them lost and went through the retry arm.
    //
    // Not the carrier for a broken retry arm: that is caught by the STATUS
    // assertion above, which fires on the loser's 500 before execution reaches
    // here — measured, that mutation panics at the "not 500" assertion. What
    // this line does catch is the counting itself going wrong: dropping the
    // `retries` increment fails here with "got 0 (attempts=2)", measured.
    assert_eq!(
        retries, 1,
        "expected exactly one loser to take the system-area retry arm, got \
         {retries} (attempts={attempts})"
    );
    assert_eq!(first.1["track_id"], second.1["track_id"]);
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM areas WHERE kind='system'").await,
        1
    );
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM tracks WHERE purpose='launchpad'").await,
        1
    );
}

// ---------------------------------------------------------------------------
// #1209 PR-2 test #18 — the two literal-SQL statements in `routes/today.rs`
// survive the `workflow_id` -> `template_id` column rename.
// ---------------------------------------------------------------------------
//
// This route is the one track-create path that writes the track column names as
// hand-written SQL strings: an `INSERT INTO tracks(... template_id, purpose,
// template_input ...)` on the mint branch and an `UPDATE tracks SET ...
// template_id=NULL ... template_input=NULL` on the adopt branch. Neither goes
// through `TRACK_SELECT_COLUMNS` or `track_create_tx`, so a missed rename in
// either one compiles cleanly, passes clippy, and fails at RUNTIME with
// `no such column`.
//
// Two branches, two independent tests, on purpose: with only one of them the
// other statement's stale column name is a green build. Mutation evidence: put
// the old column name back in exactly one of the two statements and exactly one
// of these two tests goes red.

/// Mint branch — `INSERT INTO tracks(...)` on an empty database.
#[tokio::test]
async fn today_launchpad_mint_branch_survives_the_column_rename() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let track_id = body["track_id"].as_str().expect("track id").to_string();

    // Read the two renamed columns by their new names. If the INSERT still
    // named the old columns the request above would already have failed; this
    // read additionally proves the row landed in the columns we think it did.
    let (template_id, template_input): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT template_id, template_input FROM tracks WHERE id=?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .expect("read the renamed columns off the minted launchpad");
    assert_eq!(template_id, None, "the launchpad binds no template");
    assert_eq!(
        template_input, None,
        "the launchpad carries no template input"
    );
}

/// Adopt branch — `UPDATE tracks SET ... template_id=NULL ...` over a
/// pre-existing `purpose IS NULL AND title='Today'` row.
///
/// The legacy row is built with raw SQL rather than by calling `ensure` first,
/// so this leg does NOT depend on the mint branch's INSERT. That independence
/// is what makes the mutation evidence sharp: leave the old column name in
/// exactly one of the two statements and exactly one of these two tests
/// goes red.
///
/// The row is seeded with **non-NULL** values in both renamed columns so the
/// clearing half of that UPDATE is actually exercised: against a row that was
/// already NULL, an UPDATE that never touched those columns would look
/// identical.
#[tokio::test]
async fn today_launchpad_adopt_branch_survives_the_column_rename() {
    let b = boot().await;

    // The launchpad lives in the system area; mint it directly.
    let mut tx = b.repo.pool().begin().await.unwrap();
    let area = calm_server::db::sqlite::area_create_system_tx(&mut tx)
        .await
        .expect("system area");
    tx.commit().await.unwrap();

    let track_id = "legacy-today-track".to_string();
    sqlx::query(
        "INSERT INTO tracks (id, area_id, title, sort, lifecycle, template_id, template_input, \
         created_at, updated_at) \
         VALUES (?1, ?2, 'Today', 0, 'draft', 'small-change', '{\"issue\":1209}', 1, 1)",
    )
    .bind(&track_id)
    .bind(area.id.as_str())
    .execute(b.repo.pool())
    .await
    .expect("seed a legacy Today row carrying both template columns");
    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    assert_eq!(adopted["track_id"].as_str(), Some(track_id.as_str()));

    let (purpose, template_id, template_input): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT purpose, template_id, template_input FROM tracks WHERE id=?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .expect("read the renamed columns off the adopted launchpad");
    assert_eq!(purpose.as_deref(), Some("launchpad"));
    assert_eq!(
        template_id, None,
        "adoption must clear the template binding through the renamed column"
    );
    assert_eq!(
        template_input, None,
        "adoption must clear the template input through the renamed column"
    );
}

// ---------------------------------------------------------------------------
// #1343 — `POST /api/today/launchpad/report/reset`
// ---------------------------------------------------------------------------

async fn post(
    app: axum::Router,
    uri: &str,
    actor: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::post(uri);
    if let Some(actor) = actor {
        builder = builder.header("x-calm-actor", actor);
    }
    // Harmless everywhere else; required by the conversation create, which
    // derives its card id from it.
    builder = builder.header("idempotency-key", "today-launchpad-case");
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// #1343 — reset puts today's report back to the empty state, and touches
/// nothing else.
///
/// Three things are asserted and each rules out a different way of being
/// wrong:
///
/// * the state really was non-empty first, through the production write route
///   — otherwise every assertion below is satisfied by a no-op;
/// * `report_has_noninitial_content` is `false` afterwards, read back from
///   `GET /api/today/launchpad` rather than trusted from the reset's own
///   response, because the predicate is a byte comparison the endpoint does
///   not get to grade itself on;
/// * the launchpad's conversation and its transcript survive untouched. That
///   is the whole reason this is a report action and not a "clear Today"
///   action, and a reset implemented as a card rewrite or a planner reset
///   would take the conversation with it.
#[tokio::test]
async fn resetting_todays_report_restores_the_empty_state_without_touching_conversations() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let track_id = ensured["track_id"].as_str().unwrap().to_string();

    // A conversation on the launchpad, minted through the production route, so
    // "conversations are untouched" has something to be true of.
    let (status, conversation) = post(
        b.app.clone(),
        &format!("/api/tracks/{track_id}/conversations"),
        None,
        Some(serde_json::json!({ "text": "What happened today?" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "conversation={conversation}");
    let conversation_card = conversation["id"].as_str().unwrap().to_string();
    let messages_before = count(
        &b,
        &format!(
            "SELECT COUNT(*) FROM events WHERE kind='harness.user_message.enqueued' \
             AND scope_card='{conversation_card}'"
        ),
    )
    .await;
    assert!(
        messages_before > 0,
        "the fixture must actually have a transcript to preserve"
    );

    // Written, through the route a person writes through.
    let (status, written) = post(
        b.app.clone(),
        &format!("/api/tracks/{track_id}/report"),
        None,
        Some(serde_json::json!({
            "ifDocRev": 0,
            "summary": "两个 PR",
            "body": "# 概要\n\n今天合了两个 PR。\n",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "written={written}");
    let (_, resolved) = resolve(b.app.clone()).await;
    assert_eq!(
        resolved["report_has_noninitial_content"],
        Value::Bool(true),
        "the fixture must be in the written state before the reset means \
         anything: {resolved}"
    );

    let (status, reset) = post(
        b.app.clone(),
        "/api/today/launchpad/report/reset",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reset={reset}");
    assert_eq!(reset["track_id"], Value::String(track_id.clone()));
    assert_eq!(reset["report_has_noninitial_content"], Value::Bool(false));

    let (_, resolved) = resolve(b.app.clone()).await;
    assert_eq!(
        resolved["report_has_noninitial_content"],
        Value::Bool(false),
        "the empty-state predicate is the server's own read, and it is the one \
         that has to flip: {resolved}"
    );

    assert_eq!(
        count(
            &b,
            &format!("SELECT COUNT(*) FROM cards WHERE id='{conversation_card}'")
        )
        .await,
        1,
        "the conversation card must survive a report reset"
    );
    assert_eq!(
        count(
            &b,
            &format!(
                "SELECT COUNT(*) FROM events WHERE kind='harness.user_message.enqueued' \
                 AND scope_card='{conversation_card}'"
            )
        )
        .await,
        messages_before,
        "…and so must its transcript"
    );
}

/// The reset writes through the same door as `POST /api/tracks/{id}/report`,
/// so it carries the same user-only gate.
///
/// Asserted with a declared AI actor rather than with an absent one, because
/// `Actor::to_actor_id`'s fallback maps unknown values to `User`: a gate built
/// on the typed mapping would let `ai:claude` through, which is exactly the
/// hole `require_rest_user_actor`'s raw string check exists to close.
#[tokio::test]
async fn resetting_todays_report_refuses_a_non_user_actor() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let track_id = ensured["track_id"].as_str().unwrap().to_string();
    let (_, written) = post(
        b.app.clone(),
        &format!("/api/tracks/{track_id}/report"),
        None,
        Some(serde_json::json!({
            "ifDocRev": 0, "summary": "s", "body": "# 概要\n\nx\n",
        })),
    )
    .await;
    assert_eq!(written["summary"], Value::String("s".into()), "{written}");

    let (status, body) = post(
        b.app.clone(),
        "/api/today/launchpad/report/reset",
        Some("ai:codex"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    let (_, resolved) = resolve(b.app.clone()).await;
    assert_eq!(
        resolved["report_has_noninitial_content"],
        Value::Bool(true),
        "a refused reset must not have written anything: {resolved}"
    );
}

/// No launchpad, no report, so nothing to reset — and, in particular, nothing
/// gets *created* in order to reset it.
///
/// INV-TODAYDOC-001 is why the second half matters: `ensure` materialises a
/// workspace and waits on a `planner-harness-start`, so a reset that ensured
/// first would make a destructive no-op start a harness.
#[tokio::test]
async fn resetting_without_a_launchpad_is_a_404_and_creates_nothing() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        "/api/today/launchpad/report/reset",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
    assert_eq!(count(&b, "SELECT COUNT(*) FROM tracks").await, 0);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM operations").await, 0);
}
