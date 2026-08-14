//! Calm kernel entry point.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{response::Redirect, routing::get};
use calm_server::actor::{actor_middleware, require_loopback_connect_info};
use calm_server::auth::{self, AuthConfig, AuthState};
use calm_server::config::Config;
use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::routes;
use calm_server::state::AppState;
use calm_server::ws;
use clap::Parser;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,calm_server=debug")),
        )
        .init();

    let cfg = Config::parse();
    if cfg.emit_kernel_compatibility_json {
        let compatibility = calm_server::routes::version::current_kernel_compatibility();
        println!("{}", serde_json::to_string_pretty(&compatibility)?);
        return Ok(());
    }
    warn_if_worker_hook_callback_is_not_loopback(&cfg);

    // Storage. `mock` keeps the in-memory backend for dev — it now resolves to
    // an in-memory `SqlxRepo` (`sqlite::memory:`) so dev parity with the
    // production sqlite backend is exact (cascades, FK enforcement, etc.).
    let repo: Arc<dyn Repo> = if cfg.db_url == "mock" {
        tracing::warn!(
            "calm-server starting with in-memory SqlxRepo (sqlite::memory:, non-durable)"
        );
        Arc::new(SqlxRepo::open("sqlite::memory:").await?)
    } else {
        Arc::new(SqlxRepo::open(&cfg.db_url).await?)
    };

    let state = AppState::new(&cfg, repo).await?;

    calm_server::assert_worker_sessions_card_id_complete_on_boot(&state).await?;

    // #410 — shared codex app-server boot/takeover. The shared daemon is the
    // only codex app-server path; failures are logged so boot can still bind
    // and routes surface the daemon failure when a codex card is used.
    if let Err(e) = calm_server::boot_harnesses(&state).await {
        tracing::warn!(
            error = %e,
            "spec harness boot recovery failed; continuing without recovered harness tasks"
        );
    }

    // #388 Phase 3b — reconcile non-exited terminal rows with the
    // supervisor PTY registry. No daemon binary respawn happens here.
    calm_server::reconcile_supervisor_on_boot(&state).await;

    if let Err(e) = calm_server::worker_flow::start_on_boot(&state).await {
        tracing::warn!(
            error = %e,
            "worker-flow recorder boot start failed; capture stream disabled this boot"
        );
    }

    if let Err(e) = calm_server::task_context_sweep_on_boot(&state).await {
        tracing::warn!(
            error = %e,
            "task context boot sweep failed; recovery gates remain closed"
        );
    }

    calm_server::recover_operations_on_boot(&state).await?;

    calm_server::reaper_on_boot();

    // Issue #644 PR-B — scheduler boot sweep. Must follow operation
    // recovery (design §8 boot order; asserted in `boot_order_tests`).
    calm_server::scheduler_sweep_on_boot(&state).await;

    // Optional session-recording — when `RECORD_SESSION=<path>` is set,
    // every event broadcast on the bus is appended to that file as
    // line-delimited JSON in the replay-fixture per-event shape. The
    // result is directly playable by `cargo run --bin replay`. See
    // `calm_server::replay::spawn_session_recorder` for caveats
    // (notably: actor is recorded as `"unknown"`, see design doc §6.3).
    if let Ok(path) = std::env::var("RECORD_SESSION") {
        calm_server::replay::spawn_session_recorder(&state.events, path.into());
    }

    let cors = CorsLayer::new()
        .allow_origin(
            cfg.allowed_origin
                .parse::<axum::http::HeaderValue>()
                .map_err(|e| anyhow::anyhow!("bad CALM_ALLOWED_ORIGIN: {e}"))?,
        )
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        .allow_headers(cors_allowed_headers())
        .allow_credentials(true);

    // Issue #189 — global session gate.
    //
    // We split the route tree into three buckets so the session middleware
    // is applied to exactly the protected surface:
    //   * `auth_routes`   — login/whoami/logout. Public; do NOT gate.
    //   * `public_routes` — /api/version + /api/openapi.json. Public.
    //   * `protected_routes` + WS — every REST business endpoint + the
    //     WS upgrade routes. Gated by `auth::require_session` (HTTP) /
    //     `auth::require_session_ws` (WS) so unauthenticated requests get
    //     a clean 401 / WS upgrade rejection.
    //
    // Auth config is derived from `cfg`; the boot fails fast if
    // `auth_dev_autologin = false` and no `auth_password` is set (per
    // issue #189 acceptance — operators must explicitly opt into either
    // owner credentials OR dev autologin).
    let auth_config = AuthConfig::from_config(&cfg)?;
    if auth_config.dev_autologin {
        tracing::warn!(
            "auth: DEV AUTOLOGIN is ON — every request is auto-promoted to owner. \
             Do NOT use this in production."
        );
    }
    let auth_state = AuthState::new(auth_config);

    // Scope G — REST routes carry the `X-Calm-Actor` middleware so handler
    // writes get a declared actor (user / ai:<id>).
    //
    // Issue #189 — the protected REST subtree (everything except version
    // + openapi.json + the auth endpoints themselves) sits behind the
    // session middleware. Order matters: `actor_middleware` wraps
    // BEFORE `require_session` so the session check runs first; an
    // unauthenticated request never reaches the actor-validation code.
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(actor_middleware))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));

    // Internal worker hooks — loopback callbacks from codex/Claude worker
    // subprocesses. They carry `X-Calm-Actor` but no browser session cookie,
    // so they get actor + loopback validation and stay outside the human
    // session gate.
    let internal_rest = routes::internal_router()
        .layer(axum::middleware::from_fn(actor_middleware))
        .layer(axum::middleware::from_fn(require_loopback_connect_info));

    // WS routes — issue #189 — every upgrade handshake must carry a valid
    // session cookie (cookies are sent automatically with the WS upgrade
    // GET). The `actor_middleware` layer is NOT applied here because the
    // existing convention (see `actor.rs` doc) is that WS frames don't go
    // through the write-eventized path; we only enforce auth.
    let protected_ws = ws::router().layer(axum::middleware::from_fn_with_state(
        auth_state.clone(),
        auth::require_session_ws,
    ));

    // Public REST — version + openapi.json. No session gate, no actor
    // gate.
    let public_rest = routes::public_router();

    // Auth routes — login/whoami/logout. Public; mounted as a
    // separately-stated router because they consume `AuthState`, not
    // `AppState`.
    let auth_router = auth::router().with_state(auth_state.clone());

    let mut app = axum::Router::new()
        .merge(protected_rest)
        .merge(internal_rest)
        .merge(protected_ws)
        .merge(public_rest)
        .with_state(state)
        .merge(auth_router)
        .layer(cors);

    app = mount_frontends(app, cfg.web_dist.as_deref(), cfg.fe_dist.as_deref());

    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    tracing::info!(addr = %cfg.listen, "calm-server listening");
    calm_server::spawn_hook_fallback_replay(cfg.codex_ingest_url_resolved());
    // #954 defect 4 — graceful shutdown: SIGTERM/SIGINT stops accepting and
    // drains in-flight HTTP, bounded by SHUTDOWN_DRAIN_MAX (long-lived WS
    // connections would otherwise hold the drain open past neige-app's 5s
    // stop_grace). In-flight daemon transitions get no wait and no abort: a
    // spawn transition runs seconds-minutes, a ≤3s wait almost never
    // completes it and can't abort it safely mid-reap; runtime teardown
    // aborts the detached transition task at an await point and the
    // TERM-only guard belt covers the child. sqlite WAL is durable
    // per-commit; nothing needs an explicit flush.
    //
    // INVARIANT: calm-server shutdown NEVER signals the shared codex
    // daemon; the daemon is deliberately left running for the next boot's
    // takeover (#953 re-stamp). This is why `SharedCodexAppServer` has no
    // `Drop` impl (#954) — one would fire right here, after serve returns,
    // and silently defeat takeover.
    serve_until_shutdown(
        std::future::IntoFuture::into_future(
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal()),
        ),
        shutdown_signal(),
        SHUTDOWN_DRAIN_MAX,
    )
    .await?;

    Ok(())
}

fn mount_frontends(
    mut app: axum::Router,
    web_dist: Option<&std::path::Path>,
    fe_dist: Option<&std::path::Path>,
) -> axum::Router {
    if let Some(web_dist) = web_dist {
        let index = web_dist.join("index.html");
        tracing::info!(
            web_dist = %web_dist.display(),
            "serving built web bundle under /calm/"
        );
        app = app
            .route("/", get(|| async { Redirect::temporary("/calm/") }))
            .nest_service(
                "/calm",
                ServeDir::new(web_dist).fallback(ServeFile::new(index)),
            );
    }

    if let Some(fe_dist) = fe_dist {
        let index = fe_dist.join("index.html");
        tracing::info!(
            fe_dist = %fe_dist.display(),
            "serving built next-generation frontend bundle under /next/"
        );
        app = app.nest_service(
            "/next",
            ServeDir::new(fe_dist).fallback(ServeFile::new(index)),
        );
    }

    app
}

/// #954 defect 4 — bound on the post-signal HTTP drain. A code invariant
/// under neige-app's 5s `stop_grace` default, not a knob.
const SHUTDOWN_DRAIN_MAX: std::time::Duration = std::time::Duration::from_secs(3);

/// Resolves on SIGTERM or SIGINT (ctrl_c).
async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = ctrl_c => {}
    }
}

/// Run the serve future until it finishes its graceful drain, but never
/// longer than `drain_max` past the shutdown signal — then return anyway
/// (exit code 0). Split out of `main` so the select shape is testable with
/// mock serve/shutdown futures.
async fn serve_until_shutdown<S, F>(
    serve: S,
    shutdown: F,
    drain_max: std::time::Duration,
) -> std::io::Result<()>
where
    S: std::future::Future<Output = std::io::Result<()>>,
    F: std::future::Future<Output = ()>,
{
    let drain_deadline = async {
        shutdown.await;
        tokio::time::sleep(drain_max).await;
    };
    tokio::select! {
        result = serve => result?,
        _ = drain_deadline => {
            tracing::info!("shutdown drain window elapsed; exiting");
        }
    }
    Ok(())
}

fn cors_allowed_headers() -> [axum::http::HeaderName; 2] {
    [
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderName::from_static("idempotency-key"),
    ]
}

fn warn_if_worker_hook_callback_is_not_loopback(cfg: &Config) {
    let url = cfg.codex_ingest_url_resolved();
    let Ok(uri) = url.parse::<axum::http::Uri>() else {
        return;
    };
    let Some(host) = uri.host() else {
        return;
    };
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let Ok(ip) = host.parse::<IpAddr>() else {
        return;
    };
    if !ip.is_loopback() {
        tracing::warn!(
            worker_hook_callback_url = %url,
            "worker hook callback resolves to a non-loopback address; worker hooks will be rejected by the internal hook loopback boundary. Bind CALM_LISTEN to 0.0.0.0 so the server stays LAN-reachable while workers call back over loopback, bind the server to loopback, or set CALM_CODEX_INGEST_URL to a loopback address the server actually listens on. Tracked by #362."
        );
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode};
    use clap::Parser;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    async fn response_body(app: axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
        response_body_with_method(app, Method::GET, uri).await
    }

    async fn response_body_with_method(
        app: axum::Router,
        method: Method,
        uri: &str,
    ) -> (StatusCode, Vec<u8>) {
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    #[tokio::test]
    async fn absent_fe_dist_preserves_real_api_internal_health_and_legacy_routes() {
        let web = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let legacy_index = b"legacy-index-exact\n";
        std::fs::write(web.path().join("index.html"), legacy_index).unwrap();
        std::fs::write(web.path().join("asset.txt"), b"legacy-asset-exact\n").unwrap();

        let mut cfg = calm_server::config::Config::parse_from(["calm-server"]);
        cfg.data_dir = Some(runtime.path().join("data"));
        cfg.plugins_dir = Some(runtime.path().join("plugins"));
        cfg.plugins_data_dir = Some(runtime.path().join("plugins-data"));
        let repo: Arc<dyn calm_server::db::Repo> = Arc::new(
            calm_server::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let state = calm_server::state::AppState::new(&cfg, repo).await.unwrap();
        let routes = calm_server::routes::router().with_state(state);
        let baseline = routes.clone();
        let app = super::mount_frontends(routes, Some(web.path()), None);

        for (method, uri) in [
            (Method::GET, "/api/version"),
            (Method::GET, "/health"),
            (Method::POST, "/internal/codex/hook"),
        ] {
            assert_eq!(
                response_body_with_method(app.clone(), method.clone(), uri).await,
                response_body_with_method(baseline.clone(), method, uri).await,
                "mounting the legacy frontend changed the real {uri} route",
            );
        }
        let root = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(root.headers()[axum::http::header::LOCATION], "/calm/");
        assert_eq!(
            response_body(app.clone(), "/calm/wave/deep-link").await,
            (StatusCode::OK, legacy_index.to_vec())
        );
        assert_eq!(
            response_body(app.clone(), "/calm/asset.txt").await,
            (StatusCode::OK, b"legacy-asset-exact\n".to_vec())
        );
        assert_eq!(
            response_body(app, "/next/wave/deep-link").await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn configured_fe_dist_serves_assets_and_deep_links_alongside_legacy() {
        let web = tempfile::tempdir().unwrap();
        let fe = tempfile::tempdir().unwrap();
        std::fs::write(web.path().join("index.html"), b"legacy-index\n").unwrap();
        std::fs::write(fe.path().join("index.html"), b"next-index\n").unwrap();
        std::fs::write(fe.path().join("asset.txt"), b"next-asset\n").unwrap();

        let app = super::mount_frontends(axum::Router::new(), Some(web.path()), Some(fe.path()));
        assert_eq!(
            response_body(app.clone(), "/calm/wave/deep-link").await,
            (StatusCode::OK, b"legacy-index\n".to_vec())
        );
        assert_eq!(
            response_body(app.clone(), "/next/wave/deep-link").await,
            (StatusCode::OK, b"next-index\n".to_vec())
        );
        assert_eq!(
            response_body(app, "/next/asset.txt").await,
            (StatusCode::OK, b"next-asset\n".to_vec())
        );
    }

    /// #954 defect 4 — the drain is BOUNDED: a serve future held open past
    /// the signal (long-lived WS) is abandoned `drain_max` after the
    /// shutdown signal, and main returns Ok (exit 0).
    #[tokio::test]
    async fn serve_until_shutdown_bounds_the_drain_after_signal() {
        let started = std::time::Instant::now();
        super::serve_until_shutdown(
            std::future::pending::<std::io::Result<()>>(),
            std::future::ready(()),
            Duration::from_millis(100),
        )
        .await
        .expect("bounded drain must exit cleanly");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(100) && elapsed < Duration::from_secs(2),
            "the drain bound must fire ~drain_max after the signal (took {elapsed:?})"
        );
    }

    /// A serve future that completes on its own (clean drain before the
    /// bound) returns its own result immediately.
    #[tokio::test]
    async fn serve_until_shutdown_returns_serve_result_when_drain_completes() {
        super::serve_until_shutdown(
            std::future::ready(Ok(())),
            std::future::pending::<()>(),
            Duration::from_secs(60),
        )
        .await
        .expect("completed serve must pass its result through");

        let err = super::serve_until_shutdown(
            std::future::ready::<std::io::Result<()>>(Err(std::io::Error::other("boom"))),
            std::future::pending::<()>(),
            Duration::from_secs(60),
        )
        .await
        .expect_err("serve errors must propagate");
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn cors_allows_idempotency_key_header() {
        let headers = super::cors_allowed_headers();
        assert!(headers.contains(&axum::http::header::CONTENT_TYPE));
        assert!(
            headers
                .iter()
                .any(|header| header.as_str() == "idempotency-key")
        );
    }
}
