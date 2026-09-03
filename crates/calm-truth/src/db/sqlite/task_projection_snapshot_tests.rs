//! Issue #1027 — the diagnostics predicate must read one database snapshot.
//!
//! The original read path mixed the core track/task snapshot with later
//! reference and frozen-declaration statements. A test-only seam in the real
//! predicate pauses immediately after its fact-loading statement, so each test
//! can deterministically commit a t0/t1 change before verdict evaluation. The
//! seam changes no production query or decision branch.

use std::sync::Arc;
use std::time::Duration;

use calm_types::report_blocks::tasks::TaskDeclaration;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::{
    SqlxRepo, evaluate_schedulability,
    task_projection::evaluate_schedulability_after_snapshot_for_test,
};

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
    sqlx::query("DELETE FROM tasks WHERE id='snapshot-task'")
        .execute(repo.pool())
        .await
        .expect("delete task between the fact snapshot and verdict evaluation");
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
    sqlx::query("DELETE FROM tasks WHERE id='snapshot-task'")
        .execute(repo.pool())
        .await
        .expect("leave capacity empty");
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
    sqlx::query("DELETE FROM tasks WHERE id='snapshot-task'")
        .execute(repo.pool())
        .await
        .expect("leave capacity empty");
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
        declaration_with_reference(2, "block-present", "neige://wave/destination-track#b_1234"),
        declaration_with_reference(3, "block-missing", "neige://wave/destination-track#b_dead"),
        declaration_with_reference(4, "track-missing", "neige://wave/gone#b_1234"),
        declaration_with_reference(5, "block-unspecified", "neige://wave/destination-track"),
        declaration_with_reference(6, "cross-user-card", "neige://card/cross-user-card"),
        declaration_with_reference(
            7,
            "cross-user-block",
            "neige://wave/cross-user-track#b_1234",
        ),
        declaration_with_reference(
            8,
            "cross-user-missing-block",
            "neige://wave/cross-user-track#b_dead",
        ),
        declaration_with_reference(9, "system-card", "neige://card/system-card"),
        declaration_with_reference(10, "system-block", "neige://wave/system-track#b_1234"),
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
