//! Issue #1027 — the diagnostics predicate must read one database snapshot.
//!
//! The original read path mixed the core track/task snapshot with later
//! reference and frozen-declaration statements. A test-only seam in the real
//! predicate pauses immediately after its fact-loading statement, so each test
//! can deterministically commit a t0/t1 change before verdict evaluation. The
//! seam changes no production query or decision branch.

use std::sync::Arc;
use std::time::Duration;

use calm_types::report_blocks::tasks::{
    PLANNER_DECLARATION_AUTHOR, TaskDeclaration, project_task_declarations,
};
use calm_types::report_links::format_track_destination;
use calm_types::track_report::ReportBlock;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::{
    SqlxRepo, begin_immediate_tx, evaluate_schedulability, project_tasks_tx, task_claim_pending_tx,
    task_mark_running_tx, task_projection::evaluate_schedulability_after_snapshot_for_test,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn declaration(index: usize, key: &str, goal: &str) -> TaskDeclaration {
    let block = ReportBlock {
        id: format!("b_{index:04x}"),
        kind: calm_types::report_blocks::KIND_TASK.into(),
        rev: 0,
        payload: json!({
            "key": key,
            "kind": "codex",
            "goal": goal,
            "ready": true,
            "no_gate_reason": "not needed",
            "declared_by": PLANNER_DECLARATION_AUTHOR
        }),
    };
    let (mut declarations, diagnostics) = project_task_declarations(&[block]);
    assert!(
        diagnostics.iter().all(Vec::is_empty),
        "test declaration must be valid: {diagnostics:?}"
    );
    declarations.remove(0)
}

fn changed_declaration() -> TaskDeclaration {
    let mut declaration = declaration(0, "running-key", "changed goal");
    declaration.refs = vec!["neige://card/reference-blocker".into()];
    declaration
}

fn reference_declaration() -> TaskDeclaration {
    let mut declaration = declaration(0, "new-key", "reference goal");
    declaration.refs = vec!["neige://card/reference-blocker".into()];
    declaration
}

fn declaration_with_reference(
    index: usize,
    key: &str,
    reference: impl Into<String>,
) -> TaskDeclaration {
    let mut declaration = declaration(index, key, &format!("goal {key}"));
    declaration.refs = vec![reference.into()];
    declaration
}

fn legacy_block_reference(track_id: &str, block_id: &str) -> String {
    format_track_destination(track_id, Some(block_id))
}

fn legacy_track_reference(track_id: &str) -> String {
    format_track_destination(track_id, None)
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
    let persisted = declaration(0, "running-key", "persisted goal");
    let mut tx = begin_immediate_tx(repo.pool()).await.expect("seed task tx");
    project_tasks_tx(&mut tx, "snapshot-track", &[persisted], &[vec![]])
        .await
        .expect("project pending task");
    let task_id: String =
        sqlx::query_scalar("SELECT id FROM tasks WHERE track_id='snapshot-track'")
            .fetch_one(&mut *tx)
            .await
            .expect("projected task id");
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task_id, 0, &[], false)
            .await
            .expect("claim task"),
        1
    );
    assert_eq!(
        task_mark_running_tx(&mut tx, &task_id, None, 0, 1_000)
            .await
            .expect("mark task running"),
        1
    );
    tx.commit().await.expect("commit running task");
    repo
}

async fn delete_seeded_task(repo: &SqlxRepo) {
    let deleted =
        sqlx::query("DELETE FROM tasks WHERE track_id='snapshot-track' AND key='running-key'")
            .execute(repo.pool())
            .await
            .expect("delete seeded task")
            .rows_affected();
    assert_eq!(deleted, 1, "the t0/t1 mutation must change one row");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_never_mix_inflight_state_with_a_later_frozen_scan() {
    let repo = setup().await;
    let reader_repo = Arc::clone(&repo);
    let (snapshot_loaded_tx, snapshot_loaded_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let mut conn = reader_repo
            .pool()
            .acquire()
            .await
            .expect("reader connection");
        evaluate_schedulability_after_snapshot_for_test(
            &mut conn,
            "snapshot-track",
            &[changed_declaration()],
            &[vec![]],
            true,
            async move {
                snapshot_loaded_tx.send(()).expect("test still listening");
                resume_rx.await.expect("test must resume predicate");
            },
        )
        .await
    });
    timeout(TEST_TIMEOUT, snapshot_loaded_rx)
        .await
        .expect("predicate must finish its fact statement")
        .expect("reader task must stay alive");
    delete_seeded_task(repo.as_ref()).await;
    resume_tx.send(()).expect("reader still listening");
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

    assert!(!verdict.schedulable, "changed in-flight row: {verdict:?}");
    assert_eq!(verdict.status.as_deref(), Some("running"));
    assert!(declaration_changed, "frozen row came from a later snapshot");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_never_mix_source_area_with_later_reference_targets() {
    let repo = setup().await;
    delete_seeded_task(repo.as_ref()).await;
    let reader_repo = Arc::clone(&repo);
    let (snapshot_loaded_tx, snapshot_loaded_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let mut conn = reader_repo
            .pool()
            .acquire()
            .await
            .expect("reader connection");
        evaluate_schedulability_after_snapshot_for_test(
            &mut conn,
            "snapshot-track",
            &[reference_declaration()],
            &[vec![]],
            true,
            async move {
                snapshot_loaded_tx.send(()).expect("test still listening");
                resume_rx.await.expect("test must resume predicate");
            },
        )
        .await
    });
    timeout(TEST_TIMEOUT, snapshot_loaded_rx)
        .await
        .expect("predicate must finish its fact statement")
        .expect("reader task must stay alive");
    sqlx::query(
        "UPDATE tracks SET area_id='snapshot-area-2' \
         WHERE id IN ('snapshot-track','destination-track')",
    )
    .execute(repo.pool())
    .await
    .expect("move both tracks between the fact snapshot and verdict evaluation");
    resume_tx.send(()).expect("reader still listening");
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
    delete_seeded_task(repo.as_ref()).await;
    sqlx::query("UPDATE tracks SET planner_task_ceiling=20 WHERE id='snapshot-track'")
        .execute(repo.pool())
        .await
        .expect("leave capacity for every declaration");
    sqlx::query(
        "INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) \
         VALUES('system-area','system','#000',2,'system',0,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed system area");
    sqlx::query(
        "INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at) VALUES \
         ('cross-user-track','snapshot-area-2','cross-user',2,'draft',0,0), \
         ('system-track','system-area','system',3,'draft',0,0)",
    )
    .execute(repo.pool())
    .await
    .expect("seed cross-area tracks");
    sqlx::query(
        "INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) \
         VALUES('destination-report','destination-track','track-report',1,\
         '{\"blocks\":[{\"id\":\"b_1234\"}]}','report',1,0,0,'reportcard')",
    )
    .execute(repo.pool())
    .await
    .expect("seed destination report block");
    sqlx::query(
        "INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) VALUES \
         ('cross-user-card','cross-user-track','note',0,'{}','cross-user',1,0,0,'assistant'), \
         ('cross-user-report','cross-user-track','track-report',1,\
          '{\"blocks\":[{\"id\":\"b_1234\"}]}','cross-user-report',1,0,0,'reportcard'), \
         ('system-card','system-track','note',0,'{}','system',1,0,0,'assistant'), \
         ('system-report','system-track','track-report',1,\
          '{\"blocks\":[{\"id\":\"b_1234\"}]}','system-report',1,0,0,'reportcard')",
    )
    .execute(repo.pool())
    .await
    .expect("seed cross-area reference targets");

    let declarations = vec![
        declaration_with_reference(0, "card-present", "neige://card/reference-blocker"),
        declaration_with_reference(1, "card-missing", "neige://card/gone"),
        declaration_with_reference(
            2,
            "block-present",
            legacy_block_reference("destination-track", "b_1234"),
        ),
        declaration_with_reference(
            3,
            "block-missing",
            legacy_block_reference("destination-track", "b_dead"),
        ),
        declaration_with_reference(4, "track-missing", legacy_block_reference("gone", "b_1234")),
        declaration_with_reference(
            5,
            "block-unspecified",
            legacy_track_reference("destination-track"),
        ),
        declaration_with_reference(6, "cross-user-card", "neige://card/cross-user-card"),
        declaration_with_reference(
            7,
            "cross-user-block",
            legacy_block_reference("cross-user-track", "b_1234"),
        ),
        declaration_with_reference(
            8,
            "cross-user-missing-block",
            legacy_block_reference("cross-user-track", "b_dead"),
        ),
        declaration_with_reference(9, "system-card", "neige://card/system-card"),
        declaration_with_reference(
            10,
            "system-block",
            legacy_block_reference("system-track", "b_1234"),
        ),
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
    assert_eq!(codes["cross-user-card"], ["reference_cross_area"]);
    assert_eq!(codes["cross-user-block"], ["reference_cross_area"]);
    assert_eq!(
        codes["cross-user-missing-block"],
        ["reference_cross_area"],
        "cross-area rejection must retain priority over block existence"
    );
    assert_eq!(codes["system-card"], Vec::<&str>::new());
    assert_eq!(codes["system-block"], Vec::<&str>::new());
}
