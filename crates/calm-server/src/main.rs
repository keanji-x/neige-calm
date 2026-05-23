//! Calm kernel entry point.

use std::sync::Arc;

use calm_server::actor::actor_middleware;
use calm_server::auth::{self, AuthConfig, AuthState};
use calm_server::config::Config;
use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::routes;
use calm_server::state::AppState;
use calm_server::ws;
use clap::Parser;
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,calm_server=debug")),
        )
        .init();

    let cfg = Config::parse();

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

    // #177 — boot-time orphan-revive sweep. The WS upgrade path is
    // probe-only (no auto-respawn); this is the ONLY kernel-internal
    // path that re-mounts a daemon for an existing row. Runs once
    // before the listener binds so a request that hits in flight can't
    // race the sweep.
    calm_server::revive_orphans_on_boot(&state).await;

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

    let app = axum::Router::new()
        .merge(protected_rest)
        .merge(protected_ws)
        .merge(public_rest)
        .with_state(state)
        .merge(auth_router)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    tracing::info!(addr = %cfg.listen, "calm-server listening");
    axum::serve(listener, app).await?;

    Ok(())
}
