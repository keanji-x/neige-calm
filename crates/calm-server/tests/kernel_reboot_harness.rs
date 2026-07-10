//! #840 slice (e1) — out-of-process kernel kill+reboot harness + danger-point-1.
//!
//! This is the FIRST buildable slice of the §3 crash-recovery epic. Every
//! existing boot-recovery test is *in-process*: it builds `AppState` over
//! `sqlite::memory:` and calls a recovery fn directly (see
//! `spec_harness_boot_recovery.rs`). None of them spawns the shipped binary or
//! kills a real process, so none of them proves the durable-DB kill/reboot
//! machinery actually survives a `SIGKILL`.
//!
//! This test builds that machinery for the first time:
//!   spawn the real `calm-server` binary against a **file-backed** sqlite DB in
//!   an isolated tempdir → wait until it is fully booted → `SIGKILL` it →
//!   relaunch against the **same tempdir** on a fresh port → wait until booted
//!   again → assert the rebooted kernel preserved durable state and reclaimed it
//!   exactly once, with no duplicate dispatch.
//!
//! ## Danger-point-1: snapshot preservation + codex-free lease reclaim
//!
//! Note on wording: with no codex present, `boot_harnesses` returns `Ok(0)` and
//! harness recovery is **skipped**, so the `HarnessSnapshot` survives by
//! *non-mutation* (preservation), NOT by `state_from_snapshot` reconstruction.
//! The real crash-recovery invariant this slice proves is the **exactly-once
//! workspace-lease reclaim** across a reboot. True snapshot reconstruction
//! (`state_from_snapshot`, which requires a live codex daemon) is out of scope
//! here and is NOT covered by e2/e3 either.
//!
//! Scoped (per the converged design) to the *supervisor-free* crash-recovery
//! path — NOT terminal-PTY reconcile (which needs a live calm-proc-supervisor)
//! and NOT the codex harness snapshot (which is deferred until the shared codex
//! app-server is running; `boot_harnesses` swallows that failure and returns
//! `Ok(0)` when no codex binary is present — see `lib.rs`
//! `recover_harnesses_after_daemon_boot`).
//!
//! The cheapest fully codex-free, worker-free, deterministic, DB-observable
//! crash-recovery action is the **workspace-lease boot reclaim**, the very first
//! action of `recover_operations_on_boot`
//! (`operation::driver::recover_on_boot` → `reclaim_dead_workspace_leases_on_boot`).
//! It runs unconditionally, in pure SQLite, before the HTTP listener binds — so
//! by the time `/api/version` answers 200 the reclaim has already happened.
//!
//! We seed a `held` workspace lease owned by a *stale machine boot* (a lease
//! whose `boot_id` differs from the host's `/proc/sys/kernel/random/boot_id`),
//! plus a durable `worker_sessions` row carrying a `HarnessSnapshot`. After
//! kill+reboot we assert:
//!   * the lease was reclaimed to `released`,
//!   * exactly ONE `workspace.released` event exists (the second boot re-runs
//!     recovery over the already-released row and, fenced by
//!     `state IN ('held','releasing')`, emits nothing — the exactly-once /
//!     no-duplicate-dispatch invariant across a reboot),
//!   * the seeded worker session was neither duplicated nor mutated (no codex ⇒
//!     harness recovery skipped ⇒ its durable `HarnessSnapshot` survives intact).
//!
//! ## What e1 proves vs. what e2/e3 defer
//!   * e1 (this): the spawn → file-DB → SIGKILL → relaunch → reconcile harness
//!     works; a codex-free boot reclaim is exactly-once across a reboot and the
//!     durable snapshot is preserved. Kill lands at an *arbitrary* instant
//!     (after ready), NOT a targeted window. (True `state_from_snapshot`
//!     reconstruction needs a live codex daemon and is not proven by any of
//!     e1/e2/e3 — it belongs to the real-agent stability tier.)
//!   * e2 (deferred): a `CALM_TEST_CRASH_AT` seam in the forge merge path
//!     (`complete_parked_tx`) to crash inside the "merge landed but fence not
//!     committed" window, then assert the gh-shim merge count == 1 (exactly-once
//!     merge across the crash seam).
//!   * e3 (deferred): SIGKILL while the exit-75 *held* irreversible launcher is
//!     blocked on its `_go` handshake; assert the child exits 75 having run
//!     nothing.
//!
//! ## Safety
//! This spawns REAL `calm-server` processes, so it is hard-guarded to never
//! touch prod: the DB lives in a throwaway `tempfile::tempdir()`, the port is a
//! freshly-discovered ephemeral port (asserted `!= 4040`), and codex/claude/
//! supervisor binaries are pointed at non-existent paths so no real agent or
//! shared app-server is ever launched. The child environment is **cleared and
//! rebuilt from a minimal allowlist** (`spawn_kernel`), so no inherited
//! `CALM_*` / `NEIGE_*` / `RECORD_SESSION` var can bleed in and no write can
//! escape the tempdir (`HOME`/`TMPDIR` are redirected into it). Children are
//! killed via a `Drop` guard even on panic. It is CI-safe: no external deps, and
//! it self-skips if the sandbox denies a loopback bind.

#![cfg(target_os = "linux")]

mod support;

use std::path::PathBuf;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::harness::{HarnessPhaseTag, HarnessSnapshot, Observation};
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::json;
use support::kernel_proc::launch_kernel;
use tempfile::TempDir;

/// A machine boot id that can never match the host's real
/// `/proc/sys/kernel/random/boot_id`, so the seeded lease is always treated as
/// belonging to a *previous* (dead) machine boot and is reclaimed.
const STALE_BOOT_ID: &str = "00000000-0000-0000-0000-000000000000";

const SNAPSHOT_WATERMARK: i64 = 42;

// ---------------------------------------------------------------------------
// Durable-state seeding (before the first boot) and post-reboot assertions.
// ---------------------------------------------------------------------------

struct Seeded {
    runtime_id: String,
    lease_id: String,
    card_id: String,
    wave_id: String,
}

/// Seed the file DB with (a) a durable worker-session row carrying a
/// `HarnessSnapshot`, and (b) a `held` workspace lease owned by a stale machine
/// boot — the two durable facts danger-point-1 asserts survive a reboot.
async fn seed_durable_state(db_url: &str) -> Seeded {
    let repo = SqlxRepo::open(db_url)
        .await
        .expect("open file db for seeding");

    let cove = repo
        .cove_create(NewCove {
            name: "reboot-e1".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "reboot-e1".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();

    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        SNAPSHOT_WATERMARK,
        vec![Observation::WaveGoal {
            text: "survive the reboot".into(),
        }],
    );
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-e1".into());

    let lease_id = new_id();
    let now = now_ms();

    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-e1".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now,
        },
    )
    .await
    .unwrap();

    // The held lease belonging to a stale machine boot. `lease_owner` points at
    // no operation row (LEFT JOIN → NULL owner_phase → "not recoverable"), so
    // `workspace_lease_should_reclaim_on_boot` reclaims it purely on the
    // boot_id mismatch. No filesystem dir is required — the boot reclaim path
    // only rewrites the row + emits one `workspace.released` event.
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', 'owner-none', NULL, ?5, ?6, ?6)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(format!("/tmp/neige-e1-lease/{}", card.id))
    .bind(STALE_BOOT_ID)
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();

    tx.commit().await.unwrap();

    // Drop the pool before spawning the server so the seeding connection isn't
    // holding the file open across the boot (WAL tolerates it, but this keeps
    // the ownership story clean).
    Seeded {
        runtime_id,
        lease_id,
        card_id: card.id.to_string(),
        wave_id: wave.id.to_string(),
    }
}

struct FinalState {
    lease_state: String,
    lease_released: bool,
    lease_rows: i64,
    released_events: i64,
    worker_session_rows: i64,
    snapshot_watermark: i64,
    snapshot_pending_len: usize,
}

async fn read_final_state(db_url: &str, seeded: &Seeded) -> FinalState {
    let repo = SqlxRepo::open(db_url)
        .await
        .expect("reopen file db for asserts");

    let (lease_state, released_at): (String, Option<i64>) =
        sqlx::query_as("SELECT state, released_at_ms FROM workspace_leases WHERE lease_id = ?1")
            .bind(&seeded.lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();

    let lease_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases WHERE lease_id = ?1")
            .bind(&seeded.lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();

    // Exactly-once proof: `workspace.released` is emitted once, by boot 1's
    // reclaim; boot 2 re-runs recovery over the released row and emits nothing.
    // The DB is isolated and seeds exactly one lease, so counting by kind alone
    // is unambiguous (mirrors the in-process reclaim test's assertion).
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(repo.pool())
            .await
            .unwrap();

    let worker_session_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1")
            .bind(&seeded.card_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();

    let runtime = repo
        .session_projection_by_id(&seeded.runtime_id)
        .await
        .unwrap()
        .expect("seeded worker session must still exist after reboot");
    let stored: HarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.expect("snapshot survives")).unwrap();

    FinalState {
        lease_state,
        lease_released: released_at.is_some(),
        lease_rows,
        released_events,
        worker_session_rows,
        snapshot_watermark: stored.push_watermark,
        snapshot_pending_len: stored.pending_queue.len(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_reboot_preserves_snapshot_and_reclaims_lease_without_duplicate_dispatch() {
    // ---- prod-safety hard guards (never touch the real DB / port) ---------
    let tmp: TempDir = tempfile::tempdir().expect("tempdir");
    let tmp_path: PathBuf = tmp.path().to_path_buf();
    let db_path = tmp_path.join("calm.db");
    let db_str = db_path.to_string_lossy().to_string();
    assert!(
        !db_str.contains("/.local/share/neige-calm"),
        "test DB must never be the prod DB: {db_str}"
    );
    assert!(
        tmp_path.starts_with(std::env::temp_dir())
            || tmp_path.to_string_lossy().starts_with("/tmp"),
        "test tmpdir must live under the system temp dir: {}",
        tmp_path.display()
    );
    let db_url = format!("sqlite://{db_str}?mode=rwc");

    // ---- seed durable state, then close the seeding connection ------------
    let seeded = seed_durable_state(&db_url).await;

    // ---- boot 1: spawn the real binary, wait until fully booted -----------
    let Some(mut boot1) = launch_kernel(&tmp_path, &db_path, "boot-1", &[]) else {
        return; // sandbox denied loopback bind — CI-safe skip
    };
    assert_ne!(boot1.port, 4040);

    // ---- SIGKILL at an arbitrary instant while durable state is live ------
    boot1.sigkill_and_reap();

    // ---- boot 2: relaunch against the SAME tempdir ------------------------
    // The whole tempdir (calm.db + its `-wal`/`-shm` WAL sidecars) is preserved
    // across the kill — we reuse `tmp_path`/`db_path` verbatim. `launch_kernel`
    // picks a fresh ephemeral port; we do NOT assert it differs from boot 1's,
    // because after the kill the OS allocator may legally hand back the same
    // port and a same-port reboot is perfectly valid.
    let Some(mut boot2) = launch_kernel(&tmp_path, &db_path, "boot-2", &[]) else {
        return;
    };
    boot2.sigkill_and_reap();

    // ---- assert snapshot preservation + exactly-once reclaim, no dup -------
    let state = read_final_state(&db_url, &seeded).await;

    assert_eq!(
        state.lease_state, "released",
        "the stale-boot workspace lease must be reclaimed to `released` on reboot"
    );
    assert!(
        state.lease_released,
        "reclaimed lease must have a released_at_ms timestamp"
    );
    assert_eq!(
        state.lease_rows, 1,
        "reboot must not duplicate the lease row"
    );
    assert_eq!(
        state.released_events, 1,
        "exactly ONE workspace.released event across kill+reboot: the second boot \
         re-runs recovery over the already-released row and, fenced by \
         state IN ('held','releasing'), must emit nothing (no duplicate dispatch)"
    );
    assert_eq!(
        state.worker_session_rows, 1,
        "reboot must not spawn a duplicate worker session"
    );
    assert_eq!(
        state.snapshot_watermark, SNAPSHOT_WATERMARK,
        "durable HarnessSnapshot push_watermark must survive the reboot intact \
         (no codex ⇒ harness recovery skipped ⇒ snapshot untouched)"
    );
    assert_eq!(
        state.snapshot_pending_len, 1,
        "durable HarnessSnapshot pending_queue must survive the reboot intact"
    );

    // Touch wave_id so the field is used and the compiler keeps the invariant
    // documented in `Seeded` honest.
    assert!(!seeded.wave_id.is_empty());
}
