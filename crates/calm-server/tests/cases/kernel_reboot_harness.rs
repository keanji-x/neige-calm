//! Out-of-process kernel kill+reboot harness: spawn the real `calm-server` binary against a
//! file-backed sqlite DB in an isolated tempdir, SIGKILL it once booted, relaunch on the same
//! tempdir, and assert the workspace-lease boot reclaim is exactly-once and the durable
//! `HarnessSnapshot` is preserved. The child env is cleared and rebuilt from an allowlist.

#![cfg(target_os = "linux")]

use crate::support;

use std::path::PathBuf;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::harness::{HarnessPhaseTag, HarnessSnapshot, Observation, QueueEntry};
use calm_server::model::{NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::json;
use support::kernel_proc::launch_kernel;
use tempfile::TempDir;

/// A machine boot id that can never match the host's real one, so the seeded lease is reclaimed.
const STALE_BOOT_ID: &str = "00000000-0000-0000-0000-000000000000";

const SNAPSHOT_WATERMARK: i64 = 42;

struct Seeded {
    runtime_id: String,
    lease_id: String,
    card_id: String,
    track_id: String,
}

/// Seed a durable worker-session row carrying a `HarnessSnapshot` and a `held` workspace lease
/// owned by a stale machine boot.
async fn seed_durable_state(db_url: &str) -> Seeded {
    let repo = SqlxRepo::open(db_url)
        .await
        .expect("open file db for seeding");

    let area = repo
        .area_create(NewArea {
            name: "reboot-e1".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "reboot-e1".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();

    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        SNAPSHOT_WATERMARK,
        QueueEntry::entries_from_observations_for_test(vec![Observation::TrackGoal {
            text: "survive the reboot".into(),
        }]),
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
            kind: WorkerSessionKind::SharedPlanner,
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

    // `lease_owner` points at no operation row (NULL owner_phase → "not recoverable"), so the
    // reclaim is purely on the boot_id mismatch; no filesystem dir is required.
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, track_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', 'owner-none', NULL, ?5, ?6, ?6)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(track.id.as_str())
    .bind(format!("/tmp/neige-e1-lease/{}", card.id))
    .bind(STALE_BOOT_ID)
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();

    tx.commit().await.unwrap();

    // Drop the pool before spawning the server so the seeding connection is not holding the file open.
    Seeded {
        runtime_id,
        lease_id,
        card_id: card.id.to_string(),
        track_id: track.id.to_string(),
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

    // `workspace.released` is emitted once, by boot 1's reclaim; boot 2 re-runs recovery over the
    // released row and emits nothing.
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
        snapshot_pending_len: stored.pending_observations().len(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_reboot_preserves_snapshot_and_reclaims_lease_without_duplicate_dispatch() {
    // prod-safety hard guards (never touch the real DB / port)
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

    let seeded = seed_durable_state(&db_url).await;

    // boot 1: spawn the real binary, wait until fully booted
    let Some(mut boot1) = launch_kernel(&tmp_path, &db_path, "boot-1", &[]) else {
        return; // sandbox denied loopback bind — CI-safe skip
    };
    assert_ne!(boot1.port, 4040);

    // SIGKILL at an arbitrary instant while durable state is live
    boot1.sigkill_and_reap();

    // boot 2: relaunch against the SAME tempdir (calm.db + WAL sidecars preserved). The port is
    // not asserted to differ: the OS may legally hand back the same one.
    let Some(mut boot2) = launch_kernel(&tmp_path, &db_path, "boot-2", &[]) else {
        return;
    };
    boot2.sigkill_and_reap();

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

    // Touch track_id so the field is used.
    assert!(!seeded.track_id.is_empty());
}
