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
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::replay;
use clap::Parser;

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
        println!(
            "OK: {}/{} assertions matched ({} events seeded, last id={}, file={})",
            outcome.matched.len(),
            total,
            fixture.events.len(),
            last_id,
            args.file.display()
        );
        for m in &outcome.matched {
            println!("  ok: {m}");
        }
        Ok(())
    } else {
        println!(
            "FAIL: {}/{} assertions matched ({} events seeded, file={})",
            outcome.matched.len(),
            total,
            fixture.events.len(),
            args.file.display()
        );
        for m in &outcome.matched {
            println!("  ok: {m}");
        }
        for f in &outcome.failed {
            println!("  fail: {f}");
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
    };
    let dev_routes = axum::Router::new()
        .route("/dev/reset", post(dev_reset))
        .with_state(dev_state);
    let app = axum::Router::new()
        .merge(rest_routes)
        .merge(calm_server::ws::router())
        .with_state(state)
        .merge(dev_routes);

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
    println!("calm-server (replay mode) listening on http://{addr}");
    println!(
        "  loaded {} events from {}",
        seeded_count,
        args.file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| args.file.display().to_string())
    );
    println!("  last event: {last_kind} at id={last_id}");

    axum::serve(listener, app).await?;
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
}

async fn dev_reset(State(s): State<DevResetState>) -> (StatusCode, axum::Json<serde_json::Value>) {
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
