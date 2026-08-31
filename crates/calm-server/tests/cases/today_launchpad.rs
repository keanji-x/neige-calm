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
    ));
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo,
        _tmp: tmp,
    }
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
async fn launchpad_wave_carries_an_unfrozen_attached_workspace_on_both_branches() {
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
    assert_eq!(kind, "attached");
    assert!(!path.is_empty(), "launchpad wave got a real path");
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
    assert_eq!(wave["workspace"]["kind"], "attached", "dto={dto}");
    assert_eq!(wave["workspace"]["path"], wave["cwd"], "dto={dto}");
    assert!(wave["workspace"]["frozen_at"].is_null(), "dto={dto}");

    // Branch 2 — legacy adoption. Scramble the row so the assertion cannot
    // pass on leftovers from branch 1: the adoption branch must rewrite both
    // columns through the single writer.
    sqlx::query(
        "UPDATE waves SET purpose=NULL, workspace_path='/also-scrambled', \
         workspace_kind='managed', workspace_frozen_at=NULL WHERE id=?1",
    )
    .bind(&wave_id)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, adopted) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={adopted}");
    let (kind, path, frozen_at) = workspace_row(&b.repo, &wave_id).await;
    assert_eq!(kind, "attached", "adoption re-declares the kind");
    assert_ne!(
        path, "/also-scrambled",
        "adoption actually rewrote the path"
    );
    assert_eq!(
        frozen_at, None,
        "adoption re-points, and re-pointing must never stamp frozen_at"
    );
}

/// #1147 S1 — the bound on the launchpad exception (design D9, r3.2 amendment).
///
/// `frozen_at IS NULL` means "this workspace may still be re-pointed". S1
/// creates exactly one such wave, the kernel-owned launchpad, and it lives in
/// the **system** cove. Every user-reachable wave is frozen at creation, which
/// is what keeps a future PATCH branch that forgets to check `kind` from
/// relocating a real user repository.
///
/// Stated as a property over the whole table rather than about the one wave we
/// happen to know about: any future create path that forgets to freeze shows
/// up here, whether or not anybody remembered to write a test for it.
#[tokio::test]
async fn only_system_cove_waves_may_be_unfrozen() {
    let b = boot().await;
    let (status, body) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");

    // A second, ordinary wave in a *user* cove, minted through the production
    // route — otherwise the property below is only tested against the one row
    // that motivated it.
    let cove: Value = {
        let response = b
            .app
            .clone()
            .oneshot(
                Request::post("/api/coves")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"name": "Atlas", "color": "#abc"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    };
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post("/api/waves")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "cove_id": cove["id"],
                        "title": "user wave",
                        "cwd": cwd,
                        "attach_folder": true,
                        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
                    })
                    .to_string(),
                ))
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

    let unfrozen: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT w.id, w.purpose, c.kind FROM waves w JOIN coves c ON c.id = w.cove_id \
         WHERE w.workspace_frozen_at IS NULL",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();

    assert!(
        !unfrozen.is_empty(),
        "the launchpad wave should be here; an empty set would make this \
         property vacuous"
    );
    for (id, purpose, cove_kind) in &unfrozen {
        assert_eq!(
            cove_kind, "system",
            "wave {id} (purpose={purpose}) is unfrozen but lives in a `{cove_kind}` cove. \
             `frozen_at IS NULL` means re-pointable; outside the kernel-owned system \
             cove that is a user repository waiting to be moved (design D9)."
        );
        assert_eq!(
            purpose, "launchpad",
            "wave {id} is an unfrozen system-cove wave that is not the launchpad; \
             the exception is scoped to that one wave"
        );
    }
}
