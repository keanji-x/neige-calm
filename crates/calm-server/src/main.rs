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
    if !cfg.shared_codex_prompt_cards_enabled {
        tracing::warn!(
            target: "shared_codex_daemon::flag",
            "shared_codex_prompt_cards_enabled DISABLED - fallback to legacy per-card daemon"
        );
    }
    if !cfg.shared_codex_empty_cards_enabled {
        tracing::warn!(
            target: "shared_codex_daemon::flag",
            "shared_codex_empty_cards_enabled DISABLED - fallback to legacy per-card daemon"
        );
    }
    if !cfg.shared_codex_spec_cards_enabled {
        tracing::warn!(
            target: "shared_codex_daemon::flag",
            "shared_codex_spec_cards_enabled DISABLED - fallback to legacy per-wave daemon"
        );
    }
    if !cfg.shared_codex_worker_cards_enabled {
        tracing::warn!(
            target: "shared_codex_daemon::flag",
            "shared_codex_worker_cards_enabled DISABLED - fallback to legacy per-card daemon"
        );
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

    // #410 PR4 — shared codex app-server boot/takeover. Failure is a
    // degradation path in PR4: no card routes use this daemon yet, and the
    // rollback flag can disable it while legacy per-wave app-servers continue.
    if let Err(e) = state.shared_codex_appserver.start_or_takeover().await {
        tracing::error!(
            error = %e,
            "shared codex app-server start/takeover failed; continuing boot"
        );
    }

    // #388 Phase 3b — reconcile non-exited terminal rows with the
    // supervisor PTY registry. No daemon binary respawn happens here.
    calm_server::reconcile_supervisor_on_boot(&state).await;

    // #313 problem #1 — boot-time **takeover** of in-flight spec waves.
    // Replaces the previous "kill-on-boot" sweep
    // (`reap_orphan_appserver_groups_on_boot`). For every spec card whose
    // `card_codex_threads` spec mapping exists and whose wave is not in a
    // terminal lifecycle state: re-attach (reuse the persisted live
    // app-server if its socket+pgid are still alive, else respawn fresh),
    // `initialize` + `thread/resume(<thread_id>)`, register a fresh
    // `SpecPushHandle`, then replay every persisted event with
    // `id > push_watermark` through the dispatcher's push path so the
    // spec thread catches up on what happened while the kernel was down.
    // Failures are non-fatal per wave; the kernel boot proceeds regardless.
    // Runs before the listener binds so a request landing mid-takeover
    // can't race a half-registered handle.
    calm_server::takeover_shared_spec_cards_on_boot(&state).await;
    calm_server::takeover_spec_appservers_on_boot(&state).await;

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
        .allow_headers([axum::http::header::CONTENT_TYPE])
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

    if let Some(web_dist) = &cfg.web_dist {
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

    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    tracing::info!(addr = %cfg.listen, "calm-server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
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
