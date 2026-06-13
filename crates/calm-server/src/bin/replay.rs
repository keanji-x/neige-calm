//! `cargo run --bin replay -- --file <fixture> [--serve | --assert]`
//!
//! Replay loader for sync-engine event-trace fixtures (design doc §6.3).
//! Boots an in-memory `calm-server` with the fixture's event log
//! preloaded via `Repo::log_pure_event`, then either:
//!
//!   * `--serve`  — keep the full REST + WS router running so a developer
//!     can poke the resulting state from a browser / Playwright run.
//!     Default port: `127.0.0.1:4040` (override with `--port`).
//!
//!   * `--assert` — verify the fixture's `expected` block (last event
//!     kind, layout positions) against the seeded state. Exits 0 on
//!     match, non-zero on mismatch; prints a one-line summary + per-
//!     check detail to stdout.
//!
//! The boot + seed pipeline is shared with `tests/replay_fixtures.rs`
//! via `calm_server::replay`. The binary mounts the full app router
//! (REST + WS) on top of the seeded repo so `curl /api/coves`, `curl
//! /api/waves`, etc. all work against the playback state.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use calm_server::auth::{AuthConfig, AuthState, DEFAULT_DISPLAY_NAME};
use calm_server::db::sqlite::{SqlxRepo, wave_update_tx};
use calm_server::db::write_with_events_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{WaveLifecycle, WavePatch};
use calm_server::replay;
use calm_server::wave_lifecycle::validate_transition;
use clap::Parser;
use serde::Deserialize;

macro_rules! safe_println {
    ($($arg:tt)*) => {{
        use std::io::Write as _;

        // SIGPIPE SIG_IGN stops the kernel signal, but `println!` still
        // panics on BrokenPipe; drop stdout write errors here for #628.
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
    /// Path to the fixture JSON file (e.g.
    /// `crates/calm-server/tests/fixtures/events/<name>.events.json`).
    #[arg(long)]
    file: PathBuf,

    /// Boot the server with the fixture preloaded and keep it running.
    /// Mutually exclusive with `--assert`.
    #[arg(long, conflicts_with = "assert")]
    serve: bool,

    /// Verify the fixture's `expected` block against the seeded state.
    /// Exits 0 on match, non-zero on mismatch. Mutually exclusive with
    /// `--serve`.
    #[arg(long)]
    assert: bool,

    /// Override the listen port in `--serve` mode. Defaults to 4040 —
    /// matches the regular `calm-server` default so the same web-calm
    /// dev frontend talks to the replay server without reconfiguration.
    #[arg(long, default_value_t = 4040)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // CI tests pipe stdout/stderr through Node Playwright's setup
    // worker, which exits immediately after spawning us and dropping
    // its read end of the pipes. Without this guard, the first
    // `tracing::info!` write to stderr returns EPIPE, Rust's default
    // SIGPIPE handling on stdio writes kills the process, and clients
    // see `socket hang up` mid-response (root-caused in debug PR
    // #191). Ignoring SIGPIPE makes those writes return `EPIPE` to
    // the writer; `tracing-subscriber` silently drops the failing
    // write and the server keeps serving. This applies only to the
    // replay (dev/CI) binary — production `calm-server` keeps the
    // conventional shell-idiom SIGPIPE behavior.
    #[cfg(unix)]
    {
        use nix::sys::signal::{SigHandler, Signal, signal};
        // SAFETY: setting SIG_IGN is async-signal-safe and we run it
        // before any other thread is spawned.
        unsafe {
            let _ = signal(Signal::SIGPIPE, SigHandler::SigIgn);
        }
    }

    // tracing-subscriber pulled in for `--serve` mode (the kernel
    // emits info-level logs from the routes; --assert mode is silent
    // unless something blows up). Filter mirrors `main.rs`'s default.
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

    // Wrap the fixture in `Arc` so the `/dev/reset` handler holds its own
    // cheap reference. We keep the fixture in memory (the binary is dev-
    // only and fixtures are KB-scale) rather than re-reading from disk
    // on reset — `--file` may have been edited or deleted between boot
    // and reset, and we want reset to be deterministic w.r.t. whatever
    // the original `--serve` boot loaded. See `DevResetState`.
    let fixture = Arc::new(fixture);

    // Mirror `main.rs`: honor `RECORD_SESSION=<path>` so a developer can
    // boot `--serve`, drive a few writes from a browser or curl, and
    // capture the resulting event stream into a new fixture file.
    //
    // Subscribed **after** `seed_events` so the recorded file contains
    // only operator-driven events, not the seeded fixture's. The
    // operator-driven session is the interesting artifact — the seed
    // is already on disk in the source file.
    //
    // `--assert` mode skips this branch: assertion runs are pure reads
    // after seed and emit nothing new; recording would produce an
    // empty file. (Recorder is only meaningful while the server is
    // being driven through REST/WS, which is `--serve`-only.)
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
    // Mount the full app router — both REST and WS. `--serve` mode is
    // about letting a developer / Playwright session poke the seeded
    // state interactively, so every read-side endpoint must be live.
    //
    // REST handlers extract `Actor` via `FromRequestParts`, which reads
    // a request extension that the `actor_middleware` layer populates.
    // Without that layer, any REST *write* (curl POST /api/coves, etc.)
    // 500s with "actor middleware not applied" — so we mirror main.rs
    // and attach the middleware to the REST sub-router. Callers that
    // want non-default attribution still pass `X-Calm-Actor`; absent
    // header → default `user` actor per the middleware contract.
    //
    // CORS is intentionally still skipped — `--serve` is a single-
    // developer debugging tool, not an externally reachable surface,
    // and binding the same `4040` port as the real server means the
    // dev frontend (same-origin) doesn't need CORS anyway.
    let rest_routes = calm_server::routes::router().layer(axum::middleware::from_fn(
        calm_server::actor::actor_middleware,
    ));
    // Dev-only `POST /dev/reset` sub-router. Lives outside the REST
    // sub-router so it (a) doesn't pick up the actor middleware (the
    // reset is conceptually a fresh boot, not an audited write), and
    // (b) carries its own `(repo, bus, fixture)` state independent of
    // `AppState`. The handler itself reseeds the in-memory repo from
    // the fixture loaded at `--serve` startup. See `replay::reset_from_fixture`
    // for the wipe + reseed contract. Only mounted in `--serve` (this
    // binary is itself dev-only — design doc §6.3); production
    // `calm-server` never sees this route.
    let dev_state = DevResetState {
        repo,
        bus,
        fixture: fixture.clone(),
        app: state.clone(),
    };
    let dev_routes = axum::Router::new()
        .route("/dev/reset", post(dev_reset))
        .route("/dev/force-wave-lifecycle", post(dev_force_wave_lifecycle))
        .route("/dev/force-spec-phase", post(dev_force_spec_phase))
        .with_state(dev_state);
    // Issue #189 — the production `main.rs` mounts an auth router
    // (`/api/auth/{login,whoami,logout}`) so the frontend's `SessionProvider`
    // can probe `whoami` on boot and decide whether to render the login
    // page or the app. The replay binary's `routes::router()` is the
    // legacy combined router that predates the auth split and does NOT
    // include those endpoints; without them, the frontend's whoami probe
    // 404s, throws, and parks `SessionProvider` in the `error` branch
    // (the a11y Playwright suite then times out waiting for the sidebar
    // to appear). Mount `auth::router` here with `dev_autologin = true`
    // so every request is auto-promoted to owner and whoami returns 200
    // without a session cookie — replay is dev/test-only, exactly the
    // surface dev_autologin is meant for. The `require_session`
    // middleware is intentionally NOT attached: the legacy combined
    // `routes::router()` was always reachable without a session and the
    // a11y suite relies on that no-auth surface for direct REST drives.
    let replay_auth_config = AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: DEFAULT_DISPLAY_NAME.to_string(),
    };
    let replay_auth_state = AuthState::new(replay_auth_config);
    let auth_router = calm_server::auth::router().with_state(replay_auth_state);
    let app = axum::Router::new()
        .merge(rest_routes)
        .merge(calm_server::ws::router())
        .with_state(state)
        .merge(dev_routes)
        .merge(auth_router);

    let addr = format!("127.0.0.1:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Banner mirrors the example in design doc §6.3 so the operator
    // sees the exact format the docs promise.
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

// ---------------------------------------------------------------------------
// `POST /dev/reset` — dev-only, `--serve` mode only.
//
// Why this exists: the Playwright `a11y` project spawns one replay
// binary that serves every test in the suite. Without a reset hook,
// per-test mutations (new waves, new cards, rename edits, view-mode
// toggles, …) accumulate across tests in the same run, which makes
// previously-green specs flake when their predicates collide with
// state seeded by an earlier spec. The endpoint reseeds the in-memory
// repo from the same `Fixture` the binary booted with, restoring the
// "fresh boot" starting state. Each `a11y` spec calls it from
// `beforeEach`.
//
// Scope: this binary is itself dev-only (design doc §6.3 — it has
// `--serve` and `--assert` modes, both meant for developer drives /
// CI). No additional feature flag is needed because production
// `calm-server` (the real entrypoint via `src/main.rs`) doesn't share
// this binary's routes. If we ever needed a similar surface on the
// real server it would be gated behind a `--dev` flag, not exposed
// here.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DevResetState {
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    fixture: Arc<replay::Fixture>,
    /// Shared app state used by `/dev/force-wave-lifecycle` so the
    /// forced transition writes through the same `write_with_events_typed`
    /// path as `routes::waves::update_wave` — same caches, same event bus,
    /// same role-gate enforcement.
    app: calm_server::state::AppState,
}

async fn dev_reset(State(s): State<DevResetState>) -> (StatusCode, axum::Json<serde_json::Value>) {
    // Issue #682 review — drain `/dev/force-spec-phase`-stood-up harnesses
    // BEFORE reseeding: the reseed wipes their runtime rows, and a harness
    // left registered would survive as an orphaned 50ms-tick task whose
    // persists warn forever ("runtime … not found"), accumulating across a
    // Playwright suite's per-test resets. Shutting down first (while the
    // rows still exist) keeps the final snapshot persist clean.
    let drained = replay::shutdown_registered_harnesses(&s.app).await;
    if drained > 0 {
        tracing::info!(drained, "dev reset: shut down registered spec harnesses");
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
            // Reset failure is unexpected (in-memory sqlite, no I/O) but
            // surface a structured error so the Playwright `beforeEach`
            // can fail loudly rather than continue against a half-reset
            // repo.
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

// ---------------------------------------------------------------------------
// `POST /dev/force-wave-lifecycle` — dev-only, `--serve` mode only.
//
// Issue #269 P1 — the spec daemon does NOT run in the replay binary
// (`DaemonClient::new_stub()` + `CodexClient::new_stub()`), so the spec-
// only lifecycle progressions (`planning → dispatching → working →
// reviewing → done`) can never happen organically in an a11y / replay
// run. The Playwright wave-lifecycle suite needs to drive those edges
// to assert the kernel's terminal_at stamp + WaveLifecycleChanged
// event behavior end-to-end.
//
// This handler stamps the transition as `ActorId::Kernel`, which
// `wave_lifecycle::actor_kind` classifies as `SpecAgent`. The same
// `validate_transition` + `write_with_events_typed` pipeline as
// `routes::waves::update_wave` runs — illegal edges (e.g. draft →
// done) still reject with 403, and a successful transition emits the
// same paired `WaveLifecycleChanged` + `WaveUpdated` events on the bus
// that the production path emits. The only thing this endpoint changes
// is **who** drives the edge, not whether the edge is legal.
//
// Scope: only mounted in `--serve` mode of the replay binary (this
// binary is itself dev-only — design doc §6.3). Production
// `calm-server` never sees this route. The actor middleware is
// intentionally not in front of it (mirrors `/dev/reset`); the body
// declares the transition target only, the actor is hardcoded to
// `Kernel`.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ForceLifecycleBody {
    wave_id: String,
    to: WaveLifecycle,
}

async fn dev_force_wave_lifecycle(
    State(s): State<DevResetState>,
    axum::Json(body): axum::Json<ForceLifecycleBody>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, axum::Json<serde_json::Value>)> {
    // Read the existing row outside the tx — `update_wave` does the same
    // (cove_id is immutable so a cross-tx read is safe).
    let existing = s
        .app
        .repo
        .wave_get(&body.wave_id)
        .await
        .map_err(|e| internal_err(e.into()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({
                    "ok": false,
                    "error": format!("wave {} not found", body.wave_id),
                })),
            )
        })?;

    let from = existing.lifecycle;
    let to = body.to;
    let actor = ActorId::Kernel;

    // Run the same validator as the production route — illegal kernel
    // transitions (e.g. `draft → done`) still reject so this endpoint
    // can't be used to put the wave into an impossible state.
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

    // Idempotent same-state: short-circuit without emitting any events
    // (mirrors `update_wave`'s same-state shortcut). Return the existing
    // row so the test can still inspect `terminal_at` etc.
    if from == to {
        return Ok(axum::Json(serde_json::json!({
            "ok": true,
            "wave": existing,
            "emitted_events": 0i32,
        })));
    }

    let scope = EventScope::Wave {
        wave: existing.id.clone(),
        cove: existing.cove_id.clone(),
    };
    let cove_id_for_event = existing.cove_id.clone();
    let wave_id_for_event = existing.id.clone();
    let wave_id_for_tx = body.wave_id.clone();

    let patch = WavePatch {
        lifecycle: Some(to),
        ..WavePatch::default()
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
                let wave = wave_update_tx(tx, &wave_id_for_tx, patch).await?;
                let events: Vec<(EventScope, Event)> = vec![
                    (
                        scope.clone(),
                        Event::WaveLifecycleChanged {
                            id: wave_id_for_event.clone(),
                            cove_id: cove_id_for_event.clone(),
                            from,
                            to,
                            agent_message: None,
                        },
                    ),
                    (
                        scope,
                        Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(
                            wave.clone(),
                            None,
                        )),
                    ),
                ];
                Ok((wave, events))
            })
        },
    )
    .await;

    match result {
        Ok((wave, ids)) => Ok(axum::Json(serde_json::json!({
            "ok": true,
            "wave": wave,
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

// ---------------------------------------------------------------------------
// `POST /dev/force-spec-phase` — dev-only, `--serve` mode only.
//
// Issue #682 PR-1 — the spec harness FSM can never progress organically in
// a replay run: the shared codex app-server is a stub (`is_running()` ==
// false), so the `spec-harness-start` operation submitted by `POST
// /api/waves` fails at validate and the spec card sits with no runtime row
// and no registered harness (Step-0 probe, pinned by
// `tests/replay_force_spec_phase.rs`). Playwright e2e for SpecCurrentRun
// (#676 Stop-chip seed path, #657 typing indicator) needs to drive
// `GET /spec/run` + `harness.phase.changed` anyway.
//
// The handler delegates to `calm_server::replay::force_spec_phase`
// (fixtures-gated — the `replay` [[bin]] declares
// `required-features = ["fixtures"]`): card guards mirror the production
// `/spec/*` routes (404 unknown / 403 non-spec-codex), a missing runtime
// row + harness is stood up via the boot-recovery seam, and the phase
// force itself reuses the harness run_loop's `persist_snapshot` path —
// the single write point that keeps `GET /spec/run`, the WS event, and
// the DB snapshot consistent. Forcing the same phase twice emits no
// duplicate event.
//
// Body: `{card_id, to}` with `to` as the snake_case `HarnessPhaseTag`
// (`idle`, `issuing_turn`, `turn_running`, ...). `wedged` is rejected
// with 400 — persisting it marks the runtime failed, which the active-
// runtime read path no longer projects (review finding, #684). Response:
// `{ok, card_id, runtime_id, old_phase, new_phase}`.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ForceSpecPhaseBody {
    card_id: String,
    to: calm_server::harness::HarnessPhaseTag,
}

async fn dev_force_spec_phase(
    State(s): State<DevResetState>,
    axum::Json(body): axum::Json<ForceSpecPhaseBody>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, axum::Json<serde_json::Value>)> {
    let repo: Arc<dyn calm_server::db::Repo> = s.repo.clone();
    match calm_server::replay::force_spec_phase(&s.app, repo, &body.card_id, body.to).await {
        Ok(outcome) => Ok(axum::Json(serde_json::json!({
            "ok": true,
            "card_id": outcome.card_id,
            "runtime_id": outcome.runtime_id,
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
