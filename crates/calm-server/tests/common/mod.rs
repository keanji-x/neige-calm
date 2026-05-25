//! Shared integration-test support (#293 cutover).
//!
//! Since the push cutover, `POST /api/waves` UNCONDITIONALLY boots a real
//! `codex app-server` on the create hot path (the kernel-owned spec-push
//! channel in `spec_appserver::spawn_spec_appserver`) — it runs
//! `Command::new(<codex_bin>) app-server --listen unix://<sock>`, connects
//! over WS, drives `initialize` / `thread/start` / `turn/start`, and awaits
//! the `turn/started` notification, all BEFORE returning 201. With no real
//! codex discoverable (e.g. CI, which provisions none) that spawn
//! hard-errors and the route returns 500, so every wave-create integration
//! test that asserts 201 would fail.
//!
//! The proven-faithful stand-in is the `osc-probe-child` test fixture: when
//! invoked as `codex app-server ...` it runs `appserver::run_fake_app_server`
//! (see `tests/fixtures/osc-probe-child/appserver.rs`), which binds the
//! socket and answers exactly that handshake (permessage-deflate off, emits
//! `turn/started`) — the same fixture the `theme_osc_roundtrip` tests
//! already rely on. There, the fixture is staged as `codex` via a PATH
//! symlink because the codex-cards path hard-codes the program name and runs
//! it under `sh -c codex`.
//!
//! The wave-create harnesses don't need that PATH dance: the spec-push boot
//! invokes `s.codex.codex_bin` directly, so we just point `codex_bin` at the
//! fixture binary. This is deterministic, parallel-safe (no process-global
//! `PATH`/`set_var` mutation), and needs no symlink. Prefer this over
//! installing a real codex into CI.

use calm_server::state::CodexClient;

/// Absolute path to the `osc-probe-child` fixture binary that Cargo builds
/// alongside this integration-test crate. The fixture doubles as a minimal
/// fake `codex app-server` when invoked with the `app-server` subcommand
/// (its `main` dispatches to `appserver::run_fake_app_server`). `env!`
/// expands inside whichever test crate `mod common;` is compiled into, so
/// `CARGO_BIN_EXE_osc-probe-child` is always resolvable here.
pub fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}

/// A `CodexClient` stub whose `codex_bin` points at the fake-codex fixture
/// (see [`fake_codex_bin`]). Identical to `CodexClient::new_stub()` in every
/// other respect (its per-test `codex_homes` tempdir, etc.) — we only
/// override the binary the spec-push boot will spawn, so `POST /api/waves`
/// boots the fake app-server and returns 201 without a real codex on PATH.
pub fn fake_codex_client() -> CodexClient {
    let mut c = CodexClient::new_stub();
    c.codex_bin = fake_codex_bin();
    c
}
