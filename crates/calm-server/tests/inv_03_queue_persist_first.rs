//! # INV-3 (#318 R2-B1) — spec push queue is persist-first
//!
//! `SpecPusher::push_observation` returning `Ok(PushOutcome::Enqueued)`
//! is the system's promise that the observation will be delivered to
//! codex. The dispatcher relies on this when it deliberately DOES NOT
//! advance the durable `push_watermark` on `Enqueued` (PR #315 PR4 B1)
//! — boot recovery's `events_since(watermark)` was the safety net that
//! re-delivered on the next process restart.
//!
//! INV-3 says the queue must hold its own durability rather than
//! transitively borrow the events log's. Concretely: the row backing
//! the in-memory `VecDeque<QueuedObservation>` must already be on disk
//! by the time `Ok(Enqueued)` returns to the dispatcher, so a kernel
//! crash between enqueue and the consumer task's
//! `turn/completed`-triggered flush leaves a recoverable row that a
//! fresh process can replay on its own.
//!
//! ## Scope of this test (intentionally narrow)
//!
//! `SpecPushHandle` cannot be constructed without booting a real (or
//! fake) `codex app-server`, and `SpecPushPhase` is private, so an
//! external integration test cannot drive `push_observation` itself
//! through the `Enqueue` arm. We test the durability invariant at the
//! seam that the production push path was rewired through:
//!
//!   * `Dispatcher::queue_persist_for(card_id)` builds the
//!     [`calm_server::spec_appserver::QueuePersist`] closure trio
//!     (`enqueue` / `dequeue` / `list`) the handle installs.
//!   * The `enqueue` closure persists the row BEFORE returning, so
//!     dropping the in-memory closure + repo handle and reopening from
//!     the same on-disk DB still surfaces the row via `list`.
//!   * `dequeue` removes the row idempotently.
//!
//! This pins the contract `push_observation`'s `Enqueue` arm depends
//! on (it calls `(persist.enqueue)(envelope_id, text)` BEFORE
//! `queue.push_back`) without requiring private access to the phase
//! state machine. The end-to-end behavioral test — boot a real handle
//! mid-turn, push, kill the kernel, reopen, observe replay — lives in
//! the codex-e2e suite (PR follow-up).
//!
//! Pre-fix this test cannot even compile: `spec_card_enqueue_observation`,
//! `spec_card_queued_observations`, `spec_card_dequeue_observations`,
//! and `Dispatcher::queue_persist_for` are all introduced by R2-B1.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::dispatcher::Dispatcher;
use calm_server::event::EventBus;
use calm_server::ids::CardId;
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::spec_appserver::SpecPushRegistry;
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::json;
use tempfile::TempDir;

/// File-backed sqlite the test can close + reopen. Foreign keys ON, so
/// the `card_id` FK in `spec_push_queue` is enforced (a row pointing at
/// a non-existent card row fails with `SQLITE_CONSTRAINT_FOREIGNKEY`,
/// rather than silently orphaning).
async fn open_repo(tmp: &TempDir) -> Arc<SqlxRepo> {
    let db_path = tmp.path().join("test.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    Arc::new(SqlxRepo::open(&url).await.expect("open sqlite repo"))
}

/// Mint cove + wave + card. We don't care about role here — the
/// persist surface only needs a valid `cards.id` to FK against.
async fn seed_spec_card(repo: &SqlxRepo) -> CardId {
    let cove = repo
        .cove_create(NewCove {
            name: "inv3-cove".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "inv3-wave".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .expect("create spec card");
    card.id
}

/// Build a dispatcher just to call `queue_persist_for`. Wires the
/// minimum: same repo, a fresh bus + caches + a stub `CodexClient` /
/// `DaemonClient`. None of those are reachable from the
/// `queue_persist_for` builder; they're just here because
/// `Dispatcher::spawn` requires them.
async fn dispatcher_for(repo: Arc<SqlxRepo>) -> Dispatcher {
    let bus = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let codex = Arc::new(CodexClient::new_stub());
    let daemon = Arc::new(DaemonClient {
        data_dir: std::path::PathBuf::from("/tmp/neige-inv3-noop"),
        proc_supervisor_sock: None,
    });
    let spec_push = SpecPushRegistry::new();
    Dispatcher::spawn(
        Arc::clone(&repo) as Arc<dyn Repo>,
        bus,
        card_role_cache,
        wave_cove_cache,
        codex,
        daemon,
        None, // mcp_server: not exercised by queue_persist_for
        spec_push,
        4, // permits — value irrelevant for these tests
    )
}

/// INV-3 strict: `enqueue` must persist before returning, so a
/// process-crash-then-reopen sees the row, and the list closure
/// returns the same `(envelope_id, text)` tuple the call site supplied.
///
/// Steps:
///   1. Open repo at `file://...`, mint a spec card.
///   2. Build a `QueuePersist` from `Dispatcher::queue_persist_for`.
///   3. `(persist.enqueue)(N, "obs")` → got `Some(row_id)`.
///   4. Drop the persist closure + dispatcher + repo handle. (Simulates
///      a kernel crash mid-flush — the in-memory queue dies, only
///      on-disk state survives.)
///   5. Reopen the SAME file-backed sqlite.
///   6. Call `repo.spec_card_queued_observations(card_id)` directly,
///      expect `[(_row_id, N, "obs")]`.
#[tokio::test]
async fn inv3_enqueue_survives_drop_and_reopen() {
    let tmp = TempDir::new().expect("tempdir");
    let card_id_string = {
        let repo = open_repo(&tmp).await;
        let card_id = seed_spec_card(&repo).await;
        let dispatcher = dispatcher_for(Arc::clone(&repo)).await;
        let persist = dispatcher.queue_persist_for(card_id.clone());
        let row_id = (persist.enqueue)(42, "important observation".to_string()).await;
        assert!(
            row_id.is_some(),
            "INV-3: enqueue closure must return Some(row_id) on success — \
             got None, which means the in-memory cache would be the only \
             durability surface (re-introducing the bug)"
        );
        // Drop in this order: closure → dispatcher → repo. This is the
        // "crash" — anything in memory (the in-memory `VecDeque` that
        // production's `push_observation` would have pushed at this
        // point) is gone; only the on-disk row remains.
        drop(persist);
        drop(dispatcher);
        // `repo` drops at scope end.
        card_id.as_str().to_string()
    };

    // Reopen from the same on-disk file. A fresh process sees only what
    // was committed before the "crash".
    let repo2 = open_repo(&tmp).await;
    let pending = repo2
        .spec_card_queued_observations(&card_id_string)
        .await
        .expect("read back pending");
    assert_eq!(
        pending.len(),
        1,
        "INV-3 violated: enqueue returned Some(row_id) but the durable row \
         is gone after reopen. Expected exactly one pending row, got {pending:?}"
    );
    assert_eq!(pending[0].1, 42, "INV-3: persisted envelope_id mismatch");
    assert_eq!(
        pending[0].2, "important observation",
        "INV-3: persisted text mismatch"
    );
}

/// INV-3 (dequeue side): after a successful flush the rows must be
/// removed, so a subsequent reopen does NOT see them (production
/// `flush_push_queue` calls `(persist.dequeue)(ids)` after a successful
/// coalesced `turn/start`).
///
/// Persist two rows, dequeue one, then reopen and assert exactly one
/// row remains with the right id. This catches a regression where
/// dequeue either no-ops (rows would replay forever on every boot) or
/// wipes the whole table (un-dequeued rows would silently drop).
#[tokio::test]
async fn inv3_dequeue_removes_only_named_rows() {
    let tmp = TempDir::new().expect("tempdir");
    let (card_id_string, kept_envelope_id) = {
        let repo = open_repo(&tmp).await;
        let card_id = seed_spec_card(&repo).await;
        let dispatcher = dispatcher_for(Arc::clone(&repo)).await;
        let persist = dispatcher.queue_persist_for(card_id.clone());

        let id_a = (persist.enqueue)(10, "A".to_string()).await.expect("enq A");
        let id_b = (persist.enqueue)(11, "B".to_string()).await.expect("enq B");
        assert_ne!(id_a, id_b, "row ids must be unique");

        // Simulate a successful flush that delivered A but not B.
        (persist.dequeue)(vec![id_a]).await;

        // List under the same handle: A is gone, B remains.
        let after = (persist.list)().await;
        assert_eq!(
            after.len(),
            1,
            "after dequeueing A, exactly one row (B) should remain; got {after:?}"
        );
        assert_eq!(after[0].0, id_b);
        assert_eq!(after[0].1, 11);
        assert_eq!(after[0].2, "B");

        drop(persist);
        drop(dispatcher);
        (card_id.as_str().to_string(), 11i64)
    };

    // Reopen — the dequeue must have been a real DELETE, not just an
    // in-memory bookkeeping operation.
    let repo2 = open_repo(&tmp).await;
    let pending = repo2
        .spec_card_queued_observations(&card_id_string)
        .await
        .expect("read back pending");
    assert_eq!(
        pending.len(),
        1,
        "INV-3 dequeue must be durable: after reopen, one row should still \
         be pending. got {pending:?}"
    );
    assert_eq!(
        pending[0].1, kept_envelope_id,
        "the wrong row was deleted — INV-3 dequeue violated FIFO selectivity"
    );
}

/// INV-3 (FIFO order): `list` returns rows in id-ASC order so the
/// in-memory rehydrate preserves the original enqueue order. Production
/// `SpecPushHandle::rehydrate_queue_from_persist` pushes rows back into
/// the `VecDeque` in iteration order, so a wrong list order would have
/// the consumer flush them out-of-order and corrupt the coalesced
/// `turn/start` payload sequencing.
#[tokio::test]
async fn inv3_list_returns_rows_in_enqueue_order() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = open_repo(&tmp).await;
    let card_id = seed_spec_card(&repo).await;
    let dispatcher = dispatcher_for(Arc::clone(&repo)).await;
    let persist = dispatcher.queue_persist_for(card_id.clone());

    let _ = (persist.enqueue)(100, "first".to_string()).await.unwrap();
    let _ = (persist.enqueue)(101, "second".to_string()).await.unwrap();
    let _ = (persist.enqueue)(102, "third".to_string()).await.unwrap();

    let listed = (persist.list)().await;
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].1, 100, "first row must come first");
    assert_eq!(listed[0].2, "first");
    assert_eq!(listed[1].1, 101, "second row must come second");
    assert_eq!(listed[1].2, "second");
    assert_eq!(listed[2].1, 102, "third row must come third");
    assert_eq!(listed[2].2, "third");

    // Ids are strictly increasing too (the AUTOINCREMENT contract that
    // makes "ORDER BY id ASC" semantically meaningful as "by enqueue
    // order").
    assert!(listed[0].0 < listed[1].0);
    assert!(listed[1].0 < listed[2].0);
}

/// INV-3 (cascade): deleting the spec card row cascades to its pending
/// queue rows. Mirrors the FK behaviour of `terminals`, `card_mcp_tokens`,
/// etc. — a wave teardown (which deletes the spec card via FK chain
/// from the wave) should not leave orphan queue rows that would either
/// (a) be deleted on the next FK pass anyway (race) or (b) confuse a
/// boot-takeover that finds rows for a wave that no longer exists.
#[tokio::test]
async fn inv3_card_delete_cascades_to_pending_rows() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = open_repo(&tmp).await;
    let card_id = seed_spec_card(&repo).await;
    let dispatcher = dispatcher_for(Arc::clone(&repo)).await;
    let persist = dispatcher.queue_persist_for(card_id.clone());
    let _ = (persist.enqueue)(7, "to be cascade-deleted".to_string())
        .await
        .unwrap();
    assert_eq!(
        repo.spec_card_queued_observations(card_id.as_str())
            .await
            .unwrap()
            .len(),
        1
    );

    repo.card_delete(card_id.as_str())
        .await
        .expect("delete card");

    let after = repo
        .spec_card_queued_observations(card_id.as_str())
        .await
        .expect("read back pending");
    assert!(
        after.is_empty(),
        "INV-3 cascade: card_delete must cascade to spec_push_queue rows; \
         {} rows survived",
        after.len()
    );
}
