//! Calm kernel entry point.

use std::sync::Arc;

use calm_server::actor::actor_middleware;
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

    // Scope G — REST routes carry the `X-Calm-Actor` middleware so handler
    // writes get a declared actor (user / ai:<id>). WS endpoints are
    // upgrade-style and don't write through the same path, so they don't
    // need this layer; actor on WS frames is a separate concern.
    let rest_routes = routes::router().layer(axum::middleware::from_fn(actor_middleware));

    let app = axum::Router::new()
        .merge(rest_routes)
        .merge(ws::router())
        .with_state(state)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    tracing::info!(addr = %cfg.listen, "calm-server listening");
    axum::serve(listener, app).await?;

    Ok(())
}
