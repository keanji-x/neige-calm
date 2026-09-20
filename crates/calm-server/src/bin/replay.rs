//! `cargo run --bin replay -- --file <fixture> [--serve | --assert]`
//! Replay loader for event-trace fixtures: boots an in-memory `calm-server` with the fixture preloaded, then serves the full REST + WS router or asserts the fixture's `expected` block.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use calm_server::auth::{AuthConfig, AuthState, DEFAULT_DISPLAY_NAME};
use calm_server::db::sqlite::{SqlxRepo, track_update_tx};
use calm_server::db::write_with_events_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{TrackLifecycle, TrackPatch};
use calm_server::replay;
use calm_server::track_lifecycle::validate_transition;
use clap::Parser;
use serde::Deserialize;

macro_rules! safe_println {
    ($($arg:tt)*) => {{
        use std::io::Write as _;

        // SIGPIPE is ignored, but `println!` still panics on BrokenPipe; drop stdout write errors here.
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(&mut stdout, $($arg)*);
    }};
}

#[derive(Parser, Debug)]
#[command(
    name = "calm-server-replay",
    about = "Replay an event-trace fixture into an in-memory calm-server"
)]
struct Args {
    /// Path to the fixture JSON file.
    #[arg(long)]
    file: PathBuf,

    /// Boot the server with the fixture preloaded and keep it running.
    #[arg(long, conflicts_with = "assert")]
    serve: bool,

    /// Verify the fixture's `expected` block against the seeded state; exits non-zero on mismatch.
    #[arg(long)]
    assert: bool,

    /// Listen port in `--serve` mode; 4040 matches the regular `calm-server` default.
    #[arg(long, default_value_t = 4040)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // CI pipes stdio through a Playwright setup worker that exits right after spawning us; without SIG_IGN the first stderr write gets EPIPE and the default SIGPIPE handling kills the process.
    // Dev/CI binary only — production `calm-server` keeps the conventional SIGPIPE behavior.
    #[cfg(unix)]
    {
        use nix::sys::signal::{SigHandler, Signal, signal};
        // SAFETY: setting SIG_IGN is async-signal-safe and we run it
        // before any other thread is spawned.
        unsafe {
            let _ = signal(Signal::SIGPIPE, SigHandler::SigIgn);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,calm_server=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    if !args.serve && !args.assert {
        eprintln!("error: exactly one of --serve or --assert must be provided");
        std::process::exit(2);
    }

    let fixture = match replay::load_fixture_from_path(&args.file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let (repo, bus, state) = replay::boot_in_memory().await?;
    let ids = replay::seed_events(&repo, &bus, &fixture).await?;
    let last_id = ids.last().copied().unwrap_or(0);

    if args.assert {
        return run_assert(&repo, &fixture, &args, last_id).await;
    }

    // Kept in memory so `/dev/reset` reseeds deterministically from whatever the `--serve` boot loaded, even if `--file` was edited or deleted since.
    let fixture = Arc::new(fixture);

    // Mirror `main.rs`: honor `RECORD_SESSION=<path>`. Subscribed after `seed_events` so the recorded file holds only operator-driven events; `--assert` runs emit nothing, so recording is `--serve`-only.
    if let Ok(path) = std::env::var("RECORD_SESSION") {
        replay::spawn_session_recorder(&state.events, path.into());
    }

    run_serve(state, repo, bus, fixture, &args, ids.len(), last_id).await
}

async fn run_assert(
    repo: &Arc<calm_server::db::sqlite::SqlxRepo>,
    fixture: &replay::Fixture,
    args: &Args,
    last_id: i64,
) -> anyhow::Result<()> {
    let outcome = replay::assert_expected(repo, fixture).await?;
    let total = outcome.total();
    if outcome.ok() {
        safe_println!(
            "OK: {}/{} assertions matched ({} events seeded, last id={}, file={})",
            outcome.matched.len(),
            total,
            fixture.events.len(),
            last_id,
            args.file.display()
        );
        for m in &outcome.matched {
            safe_println!("  ok: {m}");
        }
        Ok(())
    } else {
        safe_println!(
            "FAIL: {}/{} assertions matched ({} events seeded, file={})",
            outcome.matched.len(),
            total,
            fixture.events.len(),
            args.file.display()
        );
        for m in &outcome.matched {
            safe_println!("  ok: {m}");
        }
        for f in &outcome.failed {
            safe_println!("  fail: {f}");
        }
        std::process::exit(1);
    }
}

async fn run_serve(
    state: calm_server::state::AppState,
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    fixture: Arc<replay::Fixture>,
    args: &Args,
    seeded_count: usize,
    last_id: i64,
) -> anyhow::Result<()> {
    // Full app router, REST and WS. The actor middleware must be attached to the REST sub-router or every REST write 500s with "actor middleware not applied". CORS is skipped: same-origin dev tool.
    // The `/dev/*` sub-router lives outside the REST sub-router so it skips the actor middleware (a reset is a fresh boot, not an audited write) and carries its own state.
    let dev_state = DevResetState {
        repo,
        bus,
        fixture: fixture.clone(),
        app: state.clone(),
    };
    let dev_routes = axum::Router::new()
        .route("/dev/reset", post(dev_reset))
        .route(
            "/dev/force-track-lifecycle",
            post(dev_force_track_lifecycle),
        )
        .route("/dev/force-planner-phase", post(dev_force_planner_phase))
        .with_state(dev_state);
    // Mount `auth::router` with `dev_autologin = true` so the frontend's boot-time whoami probe returns 200 without a session cookie;
    // `require_session` on the REST subtree keeps `Principal` extraction working as in production and never blocks under dev_autologin.
    let replay_auth_config = AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: DEFAULT_DISPLAY_NAME.to_string(),
    };
    let replay_auth_state = AuthState::new(replay_auth_config);
    let rest_routes = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            replay_auth_state.clone(),
            calm_server::auth::require_session,
        ));
    let auth_router = calm_server::auth::router().with_state(replay_auth_state);
    let app = axum::Router::new()
        .merge(rest_routes)
        .merge(calm_server::ws::router())
        .with_state(state)
        .merge(dev_routes)
        .merge(auth_router);

    let addr = format!("127.0.0.1:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    let last_kind = if last_id > 0 {
        fixture
            .events
            .last()
            .map(|e| e.kind.as_str())
            .unwrap_or("<empty>")
    } else {
        "<empty>"
    };
    safe_println!("calm-server (replay mode) listening on http://{addr}");
    safe_println!(
        "  loaded {} events from {}",
        seeded_count,
        args.file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| args.file.display().to_string())
    );
    safe_println!("  last event: {last_kind} at id={last_id}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

// `POST /dev/reset` — dev-only, `--serve` mode only: reseeds the in-memory repo from the boot fixture so per-test mutations do not accumulate across a Playwright suite.

#[derive(Clone)]
struct DevResetState {
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    fixture: Arc<replay::Fixture>,
    /// Shared app state so the forced transition writes through the same `write_with_events_typed` path as `routes::tracks::update_track`.
    app: calm_server::state::AppState,
}

async fn dev_reset(State(s): State<DevResetState>) -> (StatusCode, axum::Json<serde_json::Value>) {
    // Drain stood-up harnesses BEFORE reseeding: the reseed wipes their runtime rows, and an orphaned harness would keep ticking and warning forever.
    let drained = replay::shutdown_registered_harnesses(&s.app).await;
    if drained > 0 {
        tracing::info!(drained, "dev reset: shut down registered planner harnesses");
    }
    match replay::reset_from_fixture(&s.repo, &s.bus, &s.fixture).await {
        Ok(ids) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "ok": true,
                "seeded": ids.len(),
                "last_id": ids.last().copied().unwrap_or(0),
            })),
        ),
        Err(e) => {
            // Reset failure is unexpected (in-memory sqlite) but surfaced structurally so a Playwright `beforeEach` fails loudly.
            tracing::error!(error = %e, "POST /dev/reset failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "ok": false,
                    "error": e.to_string(),
                })),
            )
        }
    }
}

// `POST /dev/force-track-lifecycle` — dev-only, `--serve` mode only. The planner daemon does not run here, so planner-only lifecycle edges can never happen organically.
// Stamps the transition as `ActorId::Kernel` through the same `validate_transition` + `write_with_events_typed` pipeline as the production route: it changes who drives the edge, not whether the edge is legal.

#[derive(Debug, Deserialize)]
struct ForceLifecycleBody {
    track_id: String,
    to: TrackLifecycle,
}

async fn dev_force_track_lifecycle(
    State(s): State<DevResetState>,
    axum::Json(body): axum::Json<ForceLifecycleBody>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, axum::Json<serde_json::Value>)> {
    // Read the existing row outside the tx, as `update_track` does (area_id is immutable, so a cross-tx read is safe).
    let existing = s
        .app
        .repo
        .track_get(&body.track_id)
        .await
        .map_err(|e| internal_err(e.into()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({
                    "ok": false,
                    "error": format!("track {} not found", body.track_id),
                })),
            )
        })?;

    let from = existing.lifecycle;
    let to = body.to;
    let actor = ActorId::Kernel;

    // Same validator as the production route, so this endpoint cannot put the track into an impossible state.
    if let Err(e) = validate_transition(from, to, &actor) {
        return Err((
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "ok": false,
                "error": format!("validate_transition: {e}"),
                "from": from,
                "to": to,
            })),
        ));
    }

    // Idempotent same-state: short-circuit without emitting events, mirroring `update_track`.
    if from == to {
        return Ok(axum::Json(serde_json::json!({
            "ok": true,
            "track": existing,
            "emitted_events": 0i32,
        })));
    }

    let scope = EventScope::Track {
        track: existing.id.clone(),
        area: existing.area_id.clone(),
    };
    let area_id_for_event = existing.area_id.clone();
    let track_id_for_event = existing.id.clone();
    let track_id_for_tx = body.track_id.clone();

    let patch = TrackPatch {
        lifecycle: Some(to),
        ..TrackPatch::default()
    };

    let result = write_with_events_typed(
        s.app.repo.as_ref(),
        actor,
        None,
        &s.app.events,
        s.app.write(),
        move |tx| {
            let scope = scope.clone();
            let patch = patch.clone();
            Box::pin(async move {
                let track = track_update_tx(tx, &track_id_for_tx, patch).await?;
                let events: Vec<(EventScope, Event)> = vec![
                    (
                        scope.clone(),
                        Event::TrackLifecycleChanged {
                            id: track_id_for_event.clone(),
                            area_id: area_id_for_event.clone(),
                            from,
                            to,
                            agent_message: None,
                        },
                    ),
                    (
                        scope,
                        Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(
                            track.clone(),
                            None,
                        )),
                    ),
                ];
                Ok((track, events))
            })
        },
    )
    .await;

    match result {
        Ok((track, ids)) => Ok(axum::Json(serde_json::json!({
            "ok": true,
            "track": track,
            "emitted_events": ids.len(),
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "ok": false,
                "error": e.to_string(),
            })),
        )),
    }
}

// `POST /dev/force-planner-phase` — dev-only, `--serve` mode only. The codex app-server is a stub here, so the harness FSM never progresses organically.
// Delegates to `calm_server::replay::force_planner_phase` (fixtures-gated), which reuses the harness `persist_snapshot` path; body `{card_id, to}`, `wedged` is rejected with 400.

#[derive(Debug, Deserialize)]
struct ForcePlannerPhaseBody {
    card_id: String,
    to: calm_server::harness::HarnessPhaseTag,
}

async fn dev_force_planner_phase(
    State(s): State<DevResetState>,
    axum::Json(body): axum::Json<ForcePlannerPhaseBody>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, axum::Json<serde_json::Value>)> {
    let repo: Arc<dyn calm_server::db::Repo> = s.repo.clone();
    match calm_server::replay::force_planner_phase(&s.app, repo, &body.card_id, body.to).await {
        Ok(outcome) => Ok(axum::Json(serde_json::json!({
            "ok": true,
            "card_id": outcome.card_id,
            "worker_session_id": outcome.worker_session_id,
            "old_phase": outcome.old_phase,
            "new_phase": outcome.new_phase,
        }))),
        Err(e) => Err((
            e.status(),
            axum::Json(serde_json::json!({
                "ok": false,
                "error": e.to_string(),
            })),
        )),
    }
}

fn internal_err(e: calm_server::error::CalmError) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({
            "ok": false,
            "error": e.to_string(),
        })),
    )
}
