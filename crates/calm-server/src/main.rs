//! Calm kernel entry point.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{response::Redirect, routing::get};
use calm_server::auth::{AuthConfig, AuthState};
use calm_server::config::Config;
use calm_server::routes;
use calm_server::state::AppState;
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

    // Template roster, then storage, then the state — `AppState::boot` owns
    // that order (#1635 S5): a refused `--templates-dir` exits here with no
    // database file, no WAL and no directory created. The storage policy
    // (`mock` ⇒ in-memory `SqlxRepo`, otherwise `cfg.db_url`) lives in `boot`
    // too, so the tests on it open exactly what this process opens.
    let state = AppState::boot(&cfg).await?;

    calm_server::assert_worker_sessions_card_id_complete_on_boot(&state).await?;

    // #275 / #1109 — refuse to serve on an ambiguous `area_folders`
    // table. Overlapping claims are unreachable through today's atomic
    // writer, but a pre-#275 database can hold them, and folder
    // resolution would then hand a track to an arbitrary area.
    calm_server::assert_area_folders_disjoint_on_boot(&state).await?;

    // #410 — shared codex app-server boot/takeover. The shared daemon is the
    // only codex app-server path; failures are logged so boot can still bind
    // and routes surface the daemon failure when a codex card is used.
    if let Err(e) = calm_server::boot_harnesses(&state).await {
        tracing::warn!(
            error = %e,
            "planner harness boot recovery failed; continuing without recovered harness tasks"
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

    let mut _private_ingress = None;
    let _mobile_router = if let Some(path) = &cfg.mobile_access_config {
        anyhow::ensure!(
            !auth_state.config.dev_autologin,
            "Mobile access cannot use dev autologin"
        );
        anyhow::ensure!(cfg.fe_dist.is_some(), "Mobile access requires --fe-dist");
        let mobile_config = calm_server::mobile_access::funnel::FunnelConfig::load(path)?;
        let public_router = Arc::new(mount_frontends(
            routes::public_mobile_router(state.clone(), auth_state.clone()),
            None,
            cfg.fe_dist.as_deref(),
        ));
        auth_state
            .mobile
            .configure(mobile_config, public_router.clone())
            .await;
        Some(public_router)
    } else if let Some(path) = &cfg.private_tailnet_config {
        let setup = async {
            anyhow::ensure!(
                !auth_state.config.dev_autologin,
                "Private Tailnet cannot use dev autologin"
            );
            anyhow::ensure!(cfg.fe_dist.is_some(), "Private Tailnet requires --fe-dist");
            let config =
                calm_server::mobile_access::private_tailnet::PrivateTailnetConfig::load(path)?;
            let router = Arc::new(mount_frontends(
                routes::public_mobile_router(state.clone(), auth_state.clone()),
                None,
                cfg.fe_dist.as_deref(),
            ));
            let ingress = auth_state
                .mobile
                .configure_private(config, router.clone())
                .await?;
            Ok::<_, anyhow::Error>((router, ingress))
        }
        .await;
        match setup {
            Ok((router, ingress)) => {
                _private_ingress = Some(ingress);
                Some(router)
            }
            Err(error) => {
                tracing::warn!(%error,"private Tailnet ingress unavailable; local Neige remains available");
                None
            }
        }
    } else {
        None
    };
    let mobile_shutdown = auth_state.mobile.clone();

    let mut app = routes::application_router(state, auth_state).layer(cors);

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
    let served = serve_until_shutdown(
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
    .await;

    // Keep total drain below neige-app's five-second stop grace. Parent-death
    // ownership also kills the tunnel helper if shutdown is abrupt.
    if !matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            mobile_shutdown.shutdown()
        )
        .await,
        Ok(Ok(()))
    ) {
        tracing::warn!("mobile ingress cleanup exceeded its shutdown window");
    }
    served?;

    Ok(())
}

fn mount_frontends(
    mut app: axum::Router,
    web_dist: Option<&std::path::Path>,
    fe_dist: Option<&std::path::Path>,
) -> axum::Router {
    if let Some(web_dist) = web_dist {
        tracing::warn!(web_dist = %web_dist.display(),
            "CALM_WEB_DIST is retired; /calm/ is no longer served. Use CALM_FE_DIST.");
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

    if fe_dist.is_some() {
        app = app.route("/", get(|| async { Redirect::temporary("/next/") }));
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
    use calm_server::config::Config;
    use clap::Parser;
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
    async fn configured_fe_dist_preserves_api_and_retires_legacy_routes() {
        let web = tempfile::tempdir().unwrap();
        let fe = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let legacy_index = b"legacy-index-exact\n";
        std::fs::write(web.path().join("index.html"), legacy_index).unwrap();
        std::fs::write(web.path().join("asset.txt"), b"legacy-asset-exact\n").unwrap();
        std::fs::write(fe.path().join("index.html"), b"next-index-exact\n").unwrap();

        let mut cfg = calm_server::config::Config::parse_from(["calm-server"]);
        cfg.data_dir = Some(runtime.path().join("data"));
        cfg.plugins_dir = Some(runtime.path().join("plugins"));
        cfg.plugins_data_dir = Some(runtime.path().join("plugins-data"));
        // `db_url` is the `mock` default: the boot opens `sqlite::memory:`.
        let state = calm_server::state::AppState::boot(&cfg).await.unwrap();
        let routes = calm_server::routes::router().with_state(state);
        let baseline = routes.clone();
        let app = super::mount_frontends(routes, Some(web.path()), Some(fe.path()));

        // `/api/version` carries `nowMs`, the server clock at response time
        // (#1722 S1b), so two reads of the same route differ there by design;
        // it is erased before the byte comparison, and only there.
        fn without_now_ms(
            uri: &str,
            (status, body): (StatusCode, Vec<u8>),
        ) -> (StatusCode, Vec<u8>) {
            if uri != "/api/version" {
                return (status, body);
            }
            let mut json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(json["nowMs"].is_i64(), "{uri} must carry nowMs: {json}");
            json.as_object_mut().unwrap().remove("nowMs");
            (status, serde_json::to_vec(&json).unwrap())
        }
        for (method, uri) in [
            (Method::GET, "/api/version"),
            (Method::GET, "/api/openapi.json"),
            (Method::POST, "/internal/codex/hook"),
            (Method::POST, "/internal/claude/hook"),
        ] {
            assert_eq!(
                without_now_ms(
                    uri,
                    response_body_with_method(app.clone(), method.clone(), uri).await
                ),
                without_now_ms(
                    uri,
                    response_body_with_method(baseline.clone(), method, uri).await
                ),
                "mounting the frontends changed the real {uri} route",
            );
        }
        let root = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(root.headers()[axum::http::header::LOCATION], "/next/");
        assert_eq!(
            response_body(app.clone(), "/calm/track/deep-link").await,
            (StatusCode::NOT_FOUND, vec![])
        );
        assert_eq!(
            response_body(app.clone(), "/calm/asset.txt").await,
            (StatusCode::NOT_FOUND, vec![])
        );
        assert_eq!(
            response_body(app, "/next/track/deep-link").await,
            (StatusCode::OK, b"next-index-exact\n".to_vec())
        );
    }

    /// #1635 S5 — a boot `Config` for these tests: every runtime path under
    /// `runtime`, storage at `runtime/calm.db` (an on-disk sqlite URL of the
    /// exact form neige-app configures), templates from `templates_dir`.
    fn boot_config(runtime: &std::path::Path, templates_dir: &std::path::Path) -> Config {
        let mut cfg = Config::parse_from([
            "calm-server",
            "--templates-dir",
            templates_dir.to_str().unwrap(),
        ]);
        cfg.db_url = format!("sqlite://{}?mode=rwc", runtime.join("calm.db").display());
        cfg.data_dir = Some(runtime.join("data"));
        cfg.plugins_dir = Some(runtime.join("plugins"));
        cfg.plugins_data_dir = Some(runtime.join("plugins-data"));
        cfg.workspace_root = Some(runtime.join("workspaces"));
        cfg
    }

    /// The persistent things a boot creates, none of which may exist after a
    /// refused one: the sqlite file and its WAL/shm sidecars, the plugin
    /// install/data dirs, the runtime data dir, the managed workspace root.
    fn persistent_paths(runtime: &std::path::Path) -> [std::path::PathBuf; 7] {
        [
            runtime.join("calm.db"),
            runtime.join("calm.db-wal"),
            runtime.join("calm.db-shm"),
            runtime.join("plugins"),
            runtime.join("plugins-data"),
            runtime.join("data"),
            runtime.join("workspaces"),
        ]
    }

    /// #1635 S5 — `--templates-dir` pointing at a directory with a file that
    /// does not load fails the boot **before storage exists**: `AppState::boot`
    /// (the one thing `main` calls) returns `Err` naming the file, and the
    /// database file that `cfg.db_url` names was never created — nor its WAL,
    /// nor any runtime directory. `main`'s `?` turns the `Err` into a non-zero
    /// exit. Tested on the function, not the binary; the fail-closed variants
    /// per file shape live in `templates::site_dir_tests`.
    ///
    /// `a_templates_dir_reaches_the_picker_through_the_boot` below is the
    /// positive control for the absence assertions: the same `Config` shape
    /// with a loadable directory does create `calm.db` at that path.
    #[tokio::test]
    async fn a_bad_templates_dir_fails_the_boot_before_storage_exists() {
        let runtime = tempfile::tempdir().unwrap();
        let templates = tempfile::tempdir().unwrap();
        let bad = templates.path().join("broken.md");
        std::fs::write(&bad, b"# a template file without front matter\n").unwrap();
        let cfg = boot_config(runtime.path(), templates.path());

        let error = match calm_server::state::AppState::boot(&cfg).await {
            Ok(_) => panic!("a broken operator template must fail the boot"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(&bad.display().to_string()),
            "the boot error must name the file: {error}"
        );
        assert!(
            error.contains("must open with a `+++`"),
            "and carry the loader's reason: {error}"
        );
        // Fail-closed means nothing persistent precedes the refusal: the
        // roster is validated before storage is opened and before
        // `AppState::new` creates any directory.
        for path in persistent_paths(runtime.path()) {
            assert!(
                !path.exists(),
                "{} must not exist after a refused boot",
                path.display()
            );
        }
        assert_eq!(
            std::fs::read_dir(runtime.path()).unwrap().count(),
            0,
            "the runtime directory must be untouched after a refused boot"
        );
    }

    /// #1635 S5 — the happy path through the real boot: `AppState::boot`
    /// with a loadable `--templates-dir` → `routes::router()` → the picker
    /// lists `site/x` with the file's title after the builtin entries.
    ///
    /// This is the test that holds `AppState::new`'s hand-over of the roster
    /// into `RouteState.templates`: writing `TemplateRoster::builtin()` there
    /// instead of the parameter leaves every loader test green and turns
    /// this one red (the listing then lacks `site/x`).
    #[tokio::test]
    async fn a_templates_dir_reaches_the_picker_through_the_boot() {
        let runtime = tempfile::tempdir().unwrap();
        let templates = tempfile::tempdir().unwrap();
        std::fs::write(
            templates.path().join("x.md"),
            "+++\nid = \"x\"\ntitle = \"Operator template X\"\n+++\n# Plan\n\nOperator prose.\n",
        )
        .unwrap();
        let cfg = boot_config(runtime.path(), templates.path());

        let state = calm_server::state::AppState::boot(&cfg)
            .await
            .expect("a loadable templates dir boots");
        // Positive control for the refusal test's absence assertions: this
        // is the path a successful boot creates the database at.
        assert!(
            runtime.path().join("calm.db").exists(),
            "a successful boot creates the configured sqlite file"
        );

        let app = calm_server::routes::router().with_state(state);
        let (status, body) = response_body(app, "/api/track-templates").await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let listing: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<&str> = listing
            .as_array()
            .expect("array")
            .iter()
            .map(|template| template["id"].as_str().expect("id"))
            .collect();
        let mut expected: Vec<&str> = calm_server::templates::TemplateRoster::builtin()
            .entries()
            .iter()
            .map(|template| template.key())
            .collect();
        expected.push("site/x");
        assert_eq!(
            ids, expected,
            "builtin ids in roster order, then the site id"
        );
        let site = &listing.as_array().unwrap()[expected.len() - 1];
        assert_eq!(site["title"], "Operator template X");
    }

    #[tokio::test]
    async fn frontend_root_tracks_the_configured_bundle() {
        let assets = tempfile::tempdir().unwrap();
        for (web, fe, destination) in [
            (Some(assets.path()), Some(assets.path()), "/next/"),
            (None, Some(assets.path()), "/next/"),
        ] {
            let app = super::mount_frontends(axum::Router::new(), web, fe);
            let root = app
                .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(root.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(root.headers()[axum::http::header::LOCATION], destination);
        }
    }

    #[tokio::test]
    async fn configured_fe_dist_serves_assets_and_rejects_stale_legacy_files() {
        let web = tempfile::tempdir().unwrap();
        let fe = tempfile::tempdir().unwrap();
        std::fs::write(web.path().join("index.html"), b"legacy-index\n").unwrap();
        std::fs::write(fe.path().join("index.html"), b"next-index\n").unwrap();
        std::fs::write(fe.path().join("asset.txt"), b"next-asset\n").unwrap();

        let app = super::mount_frontends(axum::Router::new(), Some(web.path()), Some(fe.path()));
        assert_eq!(
            response_body(app.clone(), "/calm/track/deep-link").await,
            (StatusCode::NOT_FOUND, vec![])
        );
        assert_eq!(
            response_body(app.clone(), "/next/track/deep-link").await,
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
