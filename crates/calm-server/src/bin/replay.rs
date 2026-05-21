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

    run_serve(state, &fixture, &args, ids.len(), last_id).await
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
    fixture: &replay::Fixture,
    args: &Args,
    seeded_count: usize,
    last_id: i64,
) -> anyhow::Result<()> {
    // Mount the full app router — both REST and WS. `--serve` mode is
    // about letting a developer / Playwright session poke the seeded
    // state interactively, so every read-side endpoint must be live.
    //
    // We skip the actor middleware + CORS layers that `main.rs` adds
    // — `--serve` is a single-developer debugging tool, not an
    // externally reachable surface. Any external poke would still need
    // to send a `X-Calm-Actor` header to write through the REST routes,
    // but reads (the common case for replay debugging) don't.
    let app = axum::Router::new()
        .merge(calm_server::routes::router())
        .merge(calm_server::ws::router())
        .with_state(state);

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
