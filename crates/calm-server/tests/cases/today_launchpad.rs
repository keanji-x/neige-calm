#![cfg(unix)]

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::{
    card_role_cache::CardRoleCache,
    db::{Repo, RepoOutOfDomain, sqlite::SqlxRepo},
    event::EventBus,
    plugin_host::{PluginHost, PluginRegistry},
    routes,
    shared_codex_appserver::SharedCodexAppServer,
    state::{AppState, CodexClient, DaemonClient, WriteContext},
    wave_cove_cache::WaveCoveCache,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    /// #1147 S2 — the managed workspace root this boot was pinned to.
    workspace_root: PathBuf,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let waves = WaveCoveCache::new();
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
        WriteContext::new(roles.clone(), waves.clone()),
    ));
    let state = AppState::from_parts(
        repo_dyn,
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(waves),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ))
    // #1147 S2 — keep every managed workspace this test mints inside the
    // sandbox, and make the root's location assertable.
    .with_workspace_root(tmp.path().join("workspaces"));
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo,
        workspace_root: tmp.path().join("workspaces"),
        _tmp: tmp,
    }
}

async fn create_cove(b: &Boot, name: &str) -> Value {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post("/api/coves")
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

async fn create_wave(b: &Boot, body: Value) -> Value {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post("/api/waves")
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
async fn first_ensure_mints_launchpad_with_all_cards_and_idle_spec() {
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
    for key in ["wave_id", "spec_card_id", "terminal_card_id", "terminal_id"] {
        assert!(
            body[key].as_str().is_some_and(|id| !id.is_empty()),
            "missing {key}: {body}"
        );
    }
    let pool = b.repo.pool();
    let purpose: String = sqlx::query_scalar("SELECT purpose FROM waves WHERE id=?1")
        .bind(body["wave_id"].as_str().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(purpose, "launchpad");
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM cards WHERE wave_id=?1 ORDER BY kind")
            .bind(body["wave_id"].as_str().unwrap())
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(kinds, ["codex", "terminal", "wave-report"]);
    let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(body["spec_card_id"].as_str().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["harness"]["pendingQueue"], serde_json::json!([]));
    assert!(payload["harness"].get("goal").is_none());
}

#[tokio::test]
async fn repeated_ensure_preserves_spec_transcript_and_ids_and_singleton() {
    let b = boot().await;
    let (first_status, first) = ensure(b.app.clone()).await;
    assert_eq!(first_status, StatusCode::CREATED);
    b.repo
        .harness_item_insert(
            "runtime",
            first["spec_card_id"].as_str().unwrap(),
            first["wave_id"].as_str().unwrap(),
            "thread",
            Some("turn"),
            Some("item"),
            Some("agent_message"),
            "item/completed",
            "{}",
        )
        .await
        .unwrap();
    let (second_status, second) = ensure(b.app.clone()).await;
    assert_eq!(second_status, StatusCode::OK, "body={second}");
    assert_eq!(second, first);
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
        .bind(first["spec_card_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(items, 1);
    let launchpads: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE purpose='launchpad'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(launchpads, 1);
}

#[tokio::test]
async fn legacy_today_adoption_resets_spec_transcript_and_preserves_terminal() {
    let b = boot().await;
    let (_, original) = ensure(b.app.clone()).await;
    sqlx::query("UPDATE waves SET purpose=NULL WHERE id=?1")
        .bind(original["wave_id"].as_str().unwrap())
        .execute(b.repo.pool())
        .await
        .unwrap();
    b.repo
        .harness_item_insert(
            "legacy-runtime",
            original["spec_card_id"].as_str().unwrap(),
            original["wave_id"].as_str().unwrap(),
            "legacy-thread",
            None,
            Some("legacy-item"),
            Some("agent_message"),
            "item/completed",
            "{}",
        )
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=?2 WHERE id=?1")
        .bind(original["spec_card_id"].as_str().unwrap())
        .bind(r#"{"schemaVersion":1,"harness":{"snapshotVersion":9,"pendingQueue":["legacy"]}}"#)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    assert_eq!(adopted, original);
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_items WHERE card_id=?1")
        .bind(original["spec_card_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(items, 0);
    let purpose: String = sqlx::query_scalar("SELECT purpose FROM waves WHERE id=?1")
        .bind(original["wave_id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(purpose, "launchpad");
    assert_eq!(adopted["terminal_card_id"], original["terminal_card_id"]);
    assert_eq!(adopted["terminal_id"], original["terminal_id"]);
}

/// #1147 S1 (design D9 + §5 test 8). The launchpad is the wave-create path
/// that bypasses `wave_create_tx` entirely — a raw `INSERT INTO waves(...)`
/// plus a raw `UPDATE`, with a hand-written `Wave` literal and two
/// hand-written SELECT column lists. That combination fails at **runtime**,
/// not at compile time (`query_as` binds columns by name), so this asserts the
/// route still answers and that both of its branches leave the workspace and
/// its `cwd` projection agreeing.
///
/// It also pins the launchpad's documented exception: this wave is
/// **never frozen**. The adopt-legacy branch re-points an existing `Today`
/// wave at the caller's `cwd`, and `ensure` is idempotent, so a stamp written
/// on the first call would be overwritten on the second — which is precisely
/// the "one-shot, monotonic" property D1 gives `frozen_at`. Leaving it NULL is
/// how re-pointing stays legal without the latch ever being violated.
#[tokio::test]
async fn launchpad_wave_carries_an_unfrozen_managed_workspace_on_both_branches() {
    async fn workspace_row(repo: &SqlxRepo, id: &str) -> (String, String, Option<i64>) {
        sqlx::query_as(
            "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id=?1",
        )
        .bind(id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
    }

    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let wave_id = body["wave_id"].as_str().unwrap().to_string();

    // Branch 1 — fresh INSERT.
    let (kind, path, frozen_at) = workspace_row(&b.repo, &wave_id).await;
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
        "the launchpad wave must stay unfrozen so the adopt branch can \
         re-point it without re-stamping (design D1 monotonicity)"
    );

    // The read path must survive the new columns (this is where a stale
    // SELECT column list detonates).
    let response = b
        .app
        .clone()
        .oneshot(
            Request::get(format!("/api/waves/{wave_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let dto: Value = serde_json::from_slice(&bytes).unwrap();
    let wave = dto.get("wave").unwrap_or(&dto);
    assert_eq!(wave["workspace"]["kind"], "managed", "dto={dto}");
    assert_eq!(wave["workspace"]["path"], wave["cwd"], "dto={dto}");
    assert!(wave["workspace"]["frozen_at"].is_null(), "dto={dto}");

    // Branch 2 — legacy adoption. Scramble the row so the assertion cannot
    // pass on leftovers from branch 1: the adoption branch must rewrite both
    // columns through the single writer.
    sqlx::query(
        "UPDATE waves SET purpose=NULL, workspace_path='/also-scrambled', \
         workspace_kind='attached', workspace_frozen_at=99 WHERE id=?1",
    )
    .bind(&wave_id)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    let (kind, path, frozen_at) = workspace_row(&b.repo, &wave_id).await;
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
/// over the whole table rather than per known wave, so any future create path
/// that mints a managed workspace somewhere else shows up here whether or not
/// somebody remembered to write a test for it — and so S5 never needs a
/// launchpad carve-out.
#[tokio::test]
async fn every_managed_wave_lives_under_the_workspace_root() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // A user cove with both shapes of wave, minted through the production
    // route: a managed one (title-only) and an attached one (explicit cwd), so
    // the property below is neither empty nor all-launchpad.
    let cove = create_cove(&b, "Atlas").await;
    let attached_dir = TempDir::new().unwrap();
    create_wave(
        &b,
        serde_json::json!({
            "cove_id": cove["id"],
            "title": "managed wave",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    create_wave(
        &b,
        serde_json::json!({
            "cove_id": cove["id"],
            "title": "attached wave",
            "cwd": attached_dir.path().to_string_lossy(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;

    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, workspace_kind, workspace_path, purpose FROM waves ORDER BY created_at, id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    let managed: Vec<_> = rows.iter().filter(|r| r.1 == "managed").collect();
    let attached: Vec<_> = rows.iter().filter(|r| r.1 == "attached").collect();
    assert!(
        managed.len() >= 2 && managed.iter().any(|r| r.3.as_deref() == Some("launchpad")),
        "the managed set must include the launchpad AND at least one ordinary \
         wave, or this property is vacuous: {rows:?}"
    );
    assert!(
        !attached.is_empty(),
        "an attached wave must exist too, otherwise the negative check below \
         could be passing because every row happens to be managed: {rows:?}"
    );
    for (id, _, path, purpose) in managed {
        assert!(
            std::path::Path::new(path).starts_with(&b.workspace_root),
            "managed wave {id} (purpose={purpose:?}) lives at {path}, outside \
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
            "attached wave {id} at {path} is inside the managed root"
        );
    }
}

/// #1147 — the bound on the "may still be re-pointed" state (design D9, r3.2
/// amendment; narrowed by the S2 review).
///
/// S1 could say "no wave outside the launchpad is unfrozen" only because every
/// wave it could mint was `attached`. S2 mints `managed` workspaces, and
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
async fn no_attached_wave_is_ever_unfrozen() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // Both shapes in a *user* cove, minted through the production route —
    // otherwise the property is only tested against the row that motivated it.
    let cove = create_cove(&b, "Atlas").await;
    let tmp = TempDir::new().unwrap();
    create_wave(
        &b,
        serde_json::json!({
            "cove_id": cove["id"],
            "title": "user wave",
            "cwd": tmp.path().to_string_lossy(),
            "attach_folder": true,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    create_wave(
        &b,
        serde_json::json!({
            "cove_id": cove["id"],
            "title": "managed wave",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;

    let rows: Vec<(String, String, Option<i64>, Option<String>, String)> = sqlx::query_as(
        "SELECT w.id, w.workspace_kind, w.workspace_frozen_at, w.purpose, c.kind \
         FROM waves w JOIN coves c ON c.id = w.cove_id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();

    // Non-vacuity, both directions.
    assert!(
        rows.iter().any(|r| r.1 == "attached"),
        "no attached wave exists, so the property below is vacuous: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.2.is_none()),
        "no unfrozen wave exists, so the property below is vacuous: {rows:?}"
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
    for (id, kind, frozen_at, purpose, cove_kind) in rows.iter().filter(|r| r.2.is_none()) {
        assert_eq!(
            kind, "managed",
            "wave {id} (purpose={purpose:?}, cove={cove_kind}, frozen_at={frozen_at:?})"
        );
    }
    assert!(
        rows.iter()
            .any(|r| r.3.as_deref() == Some("launchpad") && r.2.is_none() && r.4 == "system"),
        "the kernel-owned launchpad must still be the unfrozen system-cove \
         wave design D9 carved out: {rows:?}"
    );
}

/// #1147 S2 (design D3) — the launchpad is the fifth wave-create entry point
/// and bypasses `create_wave_structure` entirely (raw `INSERT INTO waves`).
/// Without its own materialize call every codex task on the Today panel keeps
/// dying with `spawn-failed`, which is the defect #1147 was opened on.
#[tokio::test]
async fn launchpad_workspace_is_materialized() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    let (path, kind, frozen_at, cove_id): (String, String, Option<i64>, String) = sqlx::query_as(
        "SELECT workspace_path, workspace_kind, workspace_frozen_at, cove_id FROM waves \
         WHERE purpose='launchpad'",
    )
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    // #1147 S2 review ruling ① — a managed workspace like any other, at
    // `<root>/<system_cove_id>/<wave_id>`, and still never frozen (design D9).
    assert_eq!(kind, "managed");
    assert_eq!(
        frozen_at, None,
        "the launchpad stays re-pointable; that D9 exception survives the move \
         to a managed root"
    );
    assert_eq!(
        std::path::PathBuf::from(&path),
        b.workspace_root
            .join(cove_id)
            .join(body["wave_id"].as_str().unwrap())
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
