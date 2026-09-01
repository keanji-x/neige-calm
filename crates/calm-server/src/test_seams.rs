//! Deterministic crash injection for out-of-process crash-recovery tests
//! (#840 e2/e3).
//!
//! Prod-safety contract:
//!   * `crash_point` only exists under the `fixtures` feature, which the
//!     production `calm-server` binary never enables, and every call site
//!     must be wrapped in `#[cfg(feature = "fixtures")]` — there is no
//!     unconditional stub, so a non-gated call site fails to compile in a
//!     release build. A `cargo build --release` therefore compiles zero
//!     code for the seam: no call, no argument construction, and no "an
//!     env var could crash prod" surface at all.
//!     `CARGO_BIN_EXE_calm-server` under `cargo test` IS built with
//!     `fixtures` on (the `[dev-dependencies]` self-loop in Cargo.toml), so
//!     the harness-spawned binary can reach it with zero CI plumbing.
//!   * Even in a fixtures build it is double-gated: it fires only when the
//!     process env var `CALM_TEST_CRASH_AT` equals `point` exactly. When the
//!     env var is unset (a fixtures build outside a crash test), each call
//!     costs one `env::var` lookup plus the call site's argument
//!     construction — nothing more.
//!   * It aborts rather than panics: a panic unwinds, so `Drop` impls would
//!     roll transactions back gracefully and only the calling task would die
//!     while the server keeps serving — not a crash. `abort()` kills the
//!     process instantly (SIGABRT), no destructors — SIGKILL durability
//!     semantics, deterministically placed.

/// Crash the process here iff `CALM_TEST_CRASH_AT` equals `point` exactly.
///
/// Call sites MUST be gated with `#[cfg(feature = "fixtures")]` (the whole
/// statement, so the argument expression is compiled out too) and qualify
/// `point` with enough context (e.g. the typed event kind) that a test can
/// target one specific operation without tripping on other operations
/// flowing through the same completion path.
#[cfg(feature = "fixtures")]
pub fn crash_point(point: &str) {
    if std::env::var("CALM_TEST_CRASH_AT").is_ok_and(|v| v == point) {
        eprintln!("CALM_TEST_CRASH_AT={point}: aborting for crash-recovery test");
        std::process::abort();
    }
}

/// #1147 S2 (red-team B5) — reach the production worker-lease preparation from
/// an integration test.
///
/// `prepare_workspace_lease_target_tx` is `pub(crate)`, and the point of the
/// test that uses this is that the **real** lease path repairs an
/// un-materialized managed workspace. Re-implementing the lease path in the
/// test would prove nothing about production. `fixtures`-only, like the rest of
/// this module, so the production binary compiles none of it.
#[cfg(feature = "fixtures")]
pub async fn prepare_workspace_lease_target_for_test(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wave_id: &str,
    card_id: &str,
    workspace_root: &std::path::Path,
) -> crate::error::Result<std::path::PathBuf> {
    crate::operation::workspace_lease::prepare_workspace_lease_target_tx(
        tx,
        wave_id,
        card_id,
        workspace_root,
    )
    .await
    .map(|target| target.repo_root)
}

/// #1147 S3 — reach the production workspace-lease *acquisition* from an
/// integration test.
///
/// Freeze point 1 ("the first workspace lease") lives inside
/// `acquire_workspace_lease_at_path_tx`, the single statement both public
/// `acquire_*` wrappers bottom out in. The alternative for testing it is
/// `POST /api/waves/{id}/codex-cards`, which needs a live codex app-server —
/// so the test would either be skipped in CI or would assert on a
/// re-implementation of the lease, and a fixture that re-implements the thing
/// under test proves nothing. This calls the real function.
///
/// `fixtures`-only, like the rest of this module.
#[cfg(feature = "fixtures")]
pub async fn acquire_workspace_lease_for_test(
    pool: &sqlx::SqlitePool,
    card_id: &str,
    wave_id: &str,
    lease_owner: &str,
    path: &std::path::Path,
) -> crate::error::Result<()> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        card_id,
        wave_id,
        lease_owner,
        path,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// #1147 S3 — build git commands in a test exactly the way the server does.
///
/// `neige_git_command` is `pub(crate)` and deliberately so: every git spawn on
/// the workspace path must go through it, and nothing outside the crate has a
/// reason to spawn git *as the server*. A test that probes "would the server
/// consider this path a Git work tree?" is the exception. A bare `git` there
/// answers a different question — `GIT_DIR`, `GIT_WORK_TREE`,
/// `GIT_CEILING_DIRECTORIES` and the `GIT_CONFIG_*` family are all present in
/// hook and CI environments and all redirect the answer — so a test probe that
/// does not scrub them can disagree with the server it is predicting, and a
/// precondition assertion that can be wrong about the production behaviour is
/// worse than none.
///
/// `fixtures`-only, like the rest of this module.
#[cfg(feature = "fixtures")]
pub fn neige_git_command_for_test() -> std::process::Command {
    crate::workspace_materialize::neige_git_command()
}
