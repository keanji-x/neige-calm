//! Wall-clocked proof that computing the launchpad opening briefing INSIDE the mint transaction does not
//! deadlock: its reads are autocommit statements on another pool connection and must be issued BEFORE the
//! transaction's first write (the hazardous table is `events`, not `tracks`).

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

/// The uncontended round takes tens of milliseconds; a shared-cache deadlock never returns at all.
const ROUND_BOUND: Duration = Duration::from_secs(8);

/// Each round is a fresh in-memory database; shared-cache is harsher than production WAL, which is the point.
const ROUNDS: usize = 8;

struct Boot {
    app: axum::Router,
    state: AppState,
    area_id: String,
    repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "briefing-in-mint-tx".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.area_folder_create(area.id.as_str(), "/workspace")
        .await
        .unwrap();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    repo.seed_track_area_cache(&tracks).await.unwrap();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles.clone()),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"))
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo_dyn, None,
    ));
    let app = routes::router()
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        area_id: area.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

impl Boot {
    async fn request(
        app: axum::Router,
        method: &str,
        uri: &str,
        idempotency_key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
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

    async fn create_track(&self, title: &str) -> String {
        let (status, body) = Self::request(
            self.app.clone(),
            "POST",
            "/api/tracks",
            None,
            Some(json!({
                "planner_provider": "codex",
                "area_id": self.area_id,
                "title": title,
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body={body}");
        body["id"].as_str().unwrap().to_string()
    }

    /// Rows the briefing's `ACTIVITY_QUERY` actually counts, so the join walks rows in all three tables.
    async fn seed_activity(&self, track_id: &str, n: usize) {
        for _ in 0..n {
            sqlx::query(
                "INSERT INTO events (at, actor, kind, scope_track, payload) \
                 VALUES (?1, 'owner', 'task.completed', ?2, '{}')",
            )
            .bind(now_ms())
            .bind(track_id)
            .execute(self.repo.pool())
            .await
            .unwrap();
        }
    }

    /// Written directly rather than through `ensure_today_launchpad`, which would add a second writer.
    async fn make_launchpad(&self, track_id: &str) {
        sqlx::query("UPDATE tracks SET purpose = 'launchpad' WHERE id = ?1")
            .bind(track_id)
            .execute(self.repo.pool())
            .await
            .unwrap();
    }

    async fn shutdown_harnesses(&self) {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
            .fetch_all(self.repo.pool())
            .await
            .unwrap();
        for id in ids {
            if let Some(handle) = self.state.harness.remove(&id) {
                let _ = handle.shutdown().await;
            }
        }
    }
}

/// One contended round: the mint transaction races a `DELETE /api/tracks/:id` (an IMMEDIATE writer) and a
/// second conversation create on the same track.
async fn one_round(round: usize) -> (StatusCode, StatusCode, StatusCode) {
    let b = boot().await;
    let host = b.create_track(&format!("host-{round}")).await;
    let victim = b.create_track(&format!("victim-{round}")).await;
    b.make_launchpad(&host).await;
    b.seed_activity(&victim, 3).await;
    b.seed_activity(&host, 2).await;

    let app_a = b.app.clone();
    let host_a = host.clone();
    let mint_a = tokio::spawn(async move {
        Boot::request(
            app_a,
            "POST",
            &format!("/api/tracks/{host_a}/conversations"),
            Some(&format!("briefing-a-{round}")),
            Some(json!({ "text": "first" })),
        )
        .await
    });
    let app_b = b.app.clone();
    let host_b = host.clone();
    let mint_b = tokio::spawn(async move {
        Boot::request(
            app_b,
            "POST",
            &format!("/api/tracks/{host_b}/conversations"),
            Some(&format!("briefing-b-{round}")),
            Some(json!({ "text": "second" })),
        )
        .await
    });
    let app_c = b.app.clone();
    let deleter = tokio::spawn(async move {
        Boot::request(
            app_c,
            "DELETE",
            &format!("/api/tracks/{victim}"),
            None,
            None,
        )
        .await
    });

    let started = std::time::Instant::now();
    let joined = tokio::time::timeout(ROUND_BOUND, async {
        let a = mint_a.await.unwrap();
        let bb = mint_b.await.unwrap();
        let c = deleter.await.unwrap();
        (a, bb, c)
    })
    .await;

    let ((sa, ba), (sb, bbody), (sc, cbody)) = joined.unwrap_or_else(|_| {
        panic!(
            "round {round}: the mint transaction computed the opening briefing \
             (reads on events/tracks/areas) at its top and then wrote cards/events; racing a \
             DELETE /api/tracks/:id and a second create, nothing completed within {:?}. \
             That is a lock cycle, and the first thing to check is whether the \
             briefing read has drifted BELOW the transaction's first write.",
            ROUND_BOUND
        )
    });
    let elapsed = started.elapsed();
    assert!(
        elapsed < ROUND_BOUND,
        "round {round} finished only at the bound ({elapsed:?})"
    );
    eprintln!("briefing-in-mint-tx round {round}: {sa} {sb} {sc} in {elapsed:?}");
    assert_eq!(sa, StatusCode::CREATED, "mint A body={ba}");
    assert_eq!(sb, StatusCode::CREATED, "mint B body={bbody}");
    assert_eq!(
        sc,
        StatusCode::NO_CONTENT,
        "concurrent track delete body={cbody}"
    );

    b.shutdown_harnesses().await;
    (sa, sb, sc)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn briefing_ordering_survives_contention_in_the_mint_transaction() {
    for round in 0..ROUNDS {
        one_round(round).await;
    }
}
