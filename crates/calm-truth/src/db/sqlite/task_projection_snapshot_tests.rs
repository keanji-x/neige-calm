//! Issue #1027 — the diagnostics predicate must read one database snapshot.
//!
//! The original read path mixed the core track/task snapshot with later
//! reference and frozen-declaration statements. This test pins the exact
//! interleaving without a production-code hook: an IMMEDIATE writer locks the
//! referenced card, the real predicate reads its core state and parks on that
//! card, then the writer deletes the in-flight task before releasing the card.

use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_types::report_blocks::tasks::TaskDeclaration;
use serde_json::json;
use sqlx::Connection;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::{SqlxRepo, evaluate_schedulability};

const PARK_FLOOR_RATIO: u32 = 50;
const MIN_PARK_FLOOR: Duration = Duration::from_millis(250);
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn changed_declaration() -> TaskDeclaration {
    TaskDeclaration {
        block_index: Some(0),
        block_id: "b_changed".into(),
        key: "running-key".into(),
        kind: "codex".into(),
        goal: "changed goal".into(),
        acceptance: None,
        gate: None,
        no_gate_reason: Some("not needed".into()),
        depends_on: Vec::new(),
        context: json!({}),
        cwd: None,
        priority: 0,
        refs: vec!["neige://card/reference-blocker".into()],
        declared_by: "spec".into(),
        released_by_user: false,
        spawn: "in-wave".into(),
        tombstoned_by: None,
        ready: true,
        tombstone: false,
    }
}

fn reference_declaration() -> TaskDeclaration {
    TaskDeclaration {
        block_id: "b_reference".into(),
        key: "new-key".into(),
        goal: "reference goal".into(),
        ..changed_declaration()
    }
}

fn declaration_with_reference(index: usize, key: &str, reference: &str) -> TaskDeclaration {
    TaskDeclaration {
        block_index: Some(index),
        block_id: format!("b_{index:04x}"),
        key: key.into(),
        goal: format!("goal {key}"),
        refs: vec![reference.into()],
        ..changed_declaration()
    }
}

async fn setup() -> Arc<SqlxRepo> {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.expect("open repo"));
    sqlx::query(
        "INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) \
         VALUES('snapshot-area','snapshot','#000',0,'user',0,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed area");
    sqlx::query(
        "INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) \
         VALUES('snapshot-area-2','snapshot-2','#000',1,'user',0,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed second area");
    sqlx::query(
        "INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at,\
         planner_task_ceiling,require_task_gates) \
         VALUES('snapshot-track','snapshot-area','snapshot',0,'draft',0,0,1,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed track");
    sqlx::query(
        "INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at,\
         planner_task_ceiling,require_task_gates) \
         VALUES('destination-track','snapshot-area','destination',1,'draft',0,0,1,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed destination track");
    sqlx::query(
        "INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) \
         VALUES('reference-blocker','destination-track','note',0,'{}','before',1,0,0,'assistant')",
    )
    .execute(repo.pool())
    .await
    .expect("seed reference card");
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,\
         status,declared_by,claim_context_json,context_closure_truncated,decl_ready,\
         decl_released_by_user,context_verify_failures,spawn,created_at_ms,updated_at_ms) \
         VALUES('snapshot-task','snapshot-track','running-key','codex','persisted goal','{}','[]',0,\
         'running','spec',NULL,0,1,0,0,'in-wave',0,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed in-flight task");
    repo
}

async fn evaluation_baseline(repo: &SqlxRepo, declaration: &TaskDeclaration) -> Duration {
    let mut baseline = Duration::ZERO;
    for _ in 0..5 {
        let started = Instant::now();
        let mut conn = repo.pool().acquire().await.expect("baseline connection");
        evaluate_schedulability(
            &mut conn,
            "snapshot-track",
            std::slice::from_ref(declaration),
            &[vec![]],
            true,
        )
        .await
        .expect("baseline schedulability read");
        baseline = baseline.max(started.elapsed());
    }
    baseline
}

async fn wait_until_reader_is_parked<T>(
    reader: &tokio::task::JoinHandle<T>,
    entered: oneshot::Receiver<()>,
    baseline: Duration,
) {
    timeout(TEST_TIMEOUT, entered)
        .await
        .expect("reader must acquire its connection")
        .expect("reader task must stay alive");
    let park_floor = (baseline * PARK_FLOOR_RATIO).max(MIN_PARK_FLOOR);
    let parked_at = Instant::now();
    while parked_at.elapsed() < park_floor {
        assert!(
            !reader.is_finished(),
            "reader must be parked on the card lookup"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_never_mix_inflight_state_with_a_later_frozen_scan() {
    let repo = setup().await;
    let baseline = evaluation_baseline(repo.as_ref(), &changed_declaration()).await;
    let (card_locked_tx, card_locked_rx) = oneshot::channel();
    let (delete_tx, delete_rx) = oneshot::channel();

    let writer_repo = Arc::clone(&repo);
    let writer = tokio::spawn(async move {
        let mut conn = writer_repo
            .pool()
            .acquire()
            .await
            .expect("writer connection");
        let mut tx = Connection::begin_with(&mut *conn, "BEGIN IMMEDIATE")
            .await
            .expect("begin writer");
        sqlx::query("UPDATE cards SET title='locked' WHERE id='reference-blocker'")
            .execute(&mut *tx)
            .await
            .expect("lock referenced card");
        card_locked_tx.send(()).expect("test still listening");
        delete_rx.await.expect("release writer");
        sqlx::query("DELETE FROM tasks WHERE id='snapshot-task'")
            .execute(&mut *tx)
            .await
            .expect("delete in-flight task");
        tx.commit().await.expect("commit deletion");
    });
    card_locked_rx.await.expect("writer reaches card lock");

    let reader_repo = Arc::clone(&repo);
    let (reader_entered_tx, reader_entered_rx) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let mut conn = reader_repo
            .pool()
            .acquire()
            .await
            .expect("reader connection");
        reader_entered_tx.send(()).expect("test still listening");
        evaluate_schedulability(
            &mut conn,
            "snapshot-track",
            &[changed_declaration()],
            &[vec![]],
            true,
        )
        .await
    });

    // Every statement before the reference lookup touches only tracks/tasks.
    // Holding W(cards) therefore makes a polled reader park at the exact seam
    // between the core state and the later frozen scan. Give it many orders of
    // magnitude more than the uncontended in-memory queries need, while
    // yielding continuously so this cannot pass merely because it was never
    // scheduled.
    wait_until_reader_is_parked(&reader, reader_entered_rx, baseline).await;
    delete_tx.send(()).expect("writer still listening");

    timeout(TEST_TIMEOUT, writer)
        .await
        .expect("writer must not deadlock")
        .expect("writer task");
    let verdicts = timeout(TEST_TIMEOUT, reader)
        .await
        .expect("reader must finish")
        .expect("reader task")
        .expect("schedulability read");
    let [verdict] = verdicts.as_slice() else {
        panic!("expected one verdict, got {verdicts:?}");
    };
    let declaration_changed = verdict
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "declaration_changed_in_flight");

    // Both coherent snapshots are legal:
    // - before deletion: running state + changed declaration => blocked;
    // - after deletion: no running state => the declaration may be admitted.
    // The old implementation returned the impossible third combination:
    // schedulable=true + status=running + no declaration-changed diagnostic.
    let coherent_before =
        !verdict.schedulable && verdict.status.as_deref() == Some("running") && declaration_changed;
    let coherent_after = verdict.schedulable && verdict.status.is_none() && !declaration_changed;
    assert!(
        coherent_before || coherent_after,
        "torn schedulability verdict: {verdict:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_never_mix_source_area_with_later_reference_targets() {
    let repo = setup().await;
    sqlx::query("DELETE FROM tasks WHERE id='snapshot-task'")
        .execute(repo.pool())
        .await
        .expect("leave capacity empty");
    let baseline = evaluation_baseline(repo.as_ref(), &reference_declaration()).await;
    let (card_locked_tx, card_locked_rx) = oneshot::channel();
    let (move_tracks_tx, move_tracks_rx) = oneshot::channel();

    let writer_repo = Arc::clone(&repo);
    let writer = tokio::spawn(async move {
        let mut conn = writer_repo
            .pool()
            .acquire()
            .await
            .expect("writer connection");
        let mut tx = Connection::begin_with(&mut *conn, "BEGIN IMMEDIATE")
            .await
            .expect("begin writer");
        sqlx::query("UPDATE cards SET title='locked' WHERE id='reference-blocker'")
            .execute(&mut *tx)
            .await
            .expect("lock referenced card");
        card_locked_tx.send(()).expect("test still listening");
        move_tracks_rx.await.expect("release writer");
        sqlx::query(
            "UPDATE tracks SET area_id='snapshot-area-2' \
             WHERE id IN ('snapshot-track','destination-track')",
        )
        .execute(&mut *tx)
        .await
        .expect("move both tracks together");
        tx.commit().await.expect("commit area move");
    });
    card_locked_rx.await.expect("writer reaches card lock");

    let reader_repo = Arc::clone(&repo);
    let (reader_entered_tx, reader_entered_rx) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let mut conn = reader_repo
            .pool()
            .acquire()
            .await
            .expect("reader connection");
        reader_entered_tx.send(()).expect("test still listening");
        evaluate_schedulability(
            &mut conn,
            "snapshot-track",
            &[reference_declaration()],
            &[vec![]],
            true,
        )
        .await
    });
    wait_until_reader_is_parked(&reader, reader_entered_rx, baseline).await;
    move_tracks_tx.send(()).expect("writer still listening");

    timeout(TEST_TIMEOUT, writer)
        .await
        .expect("writer must not deadlock")
        .expect("writer task");
    let verdicts = timeout(TEST_TIMEOUT, reader)
        .await
        .expect("reader must finish")
        .expect("reader task")
        .expect("schedulability read");
    let [verdict] = verdicts.as_slice() else {
        panic!("expected one verdict, got {verdicts:?}");
    };
    assert!(
        verdict.schedulable,
        "both tracks share one area before and after the move; a cross-area \
         diagnostic can only come from a torn snapshot: {verdict:?}"
    );
    assert!(
        !verdict
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "reference_cross_area"),
        "source and target area must come from one snapshot: {verdict:?}"
    );
}

#[tokio::test]
async fn snapshot_reference_materialization_preserves_reference_diagnostics() {
    let repo = setup().await;
    sqlx::query("DELETE FROM tasks WHERE id='snapshot-task'")
        .execute(repo.pool())
        .await
        .expect("leave capacity empty");
    sqlx::query("UPDATE tracks SET planner_task_ceiling=10 WHERE id='snapshot-track'")
        .execute(repo.pool())
        .await
        .expect("leave capacity for every declaration");
    sqlx::query(
        "INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) \
         VALUES('destination-report','destination-track','track-report',1,\
         '{\"blocks\":[{\"id\":\"b_1234\"}]}','report',1,0,0,'reportcard')",
    )
    .execute(repo.pool())
    .await
    .expect("seed destination report block");

    let declarations = vec![
        declaration_with_reference(0, "card-present", "neige://card/reference-blocker"),
        declaration_with_reference(1, "card-missing", "neige://card/gone"),
        declaration_with_reference(2, "block-present", "neige://wave/destination-track#b_1234"),
        declaration_with_reference(3, "block-missing", "neige://wave/destination-track#b_dead"),
        declaration_with_reference(4, "track-missing", "neige://wave/gone#b_1234"),
        declaration_with_reference(5, "block-unspecified", "neige://wave/destination-track"),
    ];
    let mut conn = repo.pool().acquire().await.expect("reader connection");
    let verdicts = evaluate_schedulability(
        &mut conn,
        "snapshot-track",
        &declarations,
        &vec![vec![]; declarations.len()],
        true,
    )
    .await
    .expect("evaluate references");
    let codes = verdicts
        .iter()
        .map(|verdict| {
            (
                verdict.key.as_str(),
                verdict
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.path == "refs")
                    .map(|diagnostic| diagnostic.code.as_str())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(codes["card-present"], Vec::<&str>::new());
    assert_eq!(codes["card-missing"], ["reference_missing"]);
    assert_eq!(codes["block-present"], Vec::<&str>::new());
    assert_eq!(codes["block-missing"], ["reference_missing"]);
    assert_eq!(codes["track-missing"], ["reference_missing"]);
    assert_eq!(codes["block-unspecified"], ["reference_needs_block"]);
}
