//! #985 slice 6 PR-B — acceptance for the tree-level budget: the
//! deterministic quota split in `evaluate_schedulability`, the fail-closed
//! root resolution, the non-tree short circuit, and the downward CTE's
//! termination guard.
//!
//! Everything here drives the production functions
//! (`wave_create_tx`, `wave_update_tx`, `wave_tree_term`,
//! `evaluate_schedulability`, `project_tasks_tx`); no fixture re-implements
//! the predicate under test.

use std::time::{Duration, Instant};

use calm_types::report_blocks::tasks::TaskDeclaration;
use serde_json::json;

use super::wave_tree::{
    MAX_TREE_TASK_BUDGET, MAX_WAVE_TREE_DEPTH, TreeShare, WaveTreeTerm, deterministic_share,
    wave_tree_term,
};
use super::{SqlxRepo, evaluate_schedulability, project_tasks_tx, wave_create_tx, wave_update_tx};
use crate::model::{NewCove, NewWave, RequestTheme, WavePatch};

use super::cove_create_tx;

async fn seed_cove(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "tree".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    cove.id.to_string()
}

/// Production wave creation. Every wave in these tests is born through the
/// same writer the `child-wave` operation uses.
async fn seed_wave(repo: &SqlxRepo, cove_id: &str, title: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove_id.to_string().into(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    wave.id.to_string()
}

async fn link(repo: &SqlxRepo, child: &str, parent: &str) {
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
        .bind(parent)
        .bind(child)
        .execute(repo.pool())
        .await
        .unwrap();
}

/// `created_at` is the primary key of the quota order. Waves minted inside one
/// millisecond would otherwise tie-break on the random id, which makes the
/// EXPECTED order unknowable to the test (not to the code).
async fn stamp_created_at(repo: &SqlxRepo, wave: &str, created_at: i64) {
    sqlx::query("UPDATE waves SET created_at=?1 WHERE id=?2")
        .bind(created_at)
        .bind(wave)
        .execute(repo.pool())
        .await
        .unwrap();
}

async fn set_ceiling(repo: &SqlxRepo, wave: &str, ceiling: i64) {
    sqlx::query("UPDATE waves SET spec_task_ceiling=?1 WHERE id=?2")
        .bind(ceiling)
        .bind(wave)
        .execute(repo.pool())
        .await
        .unwrap();
}

async fn set_tree_budget(repo: &SqlxRepo, root: &str, budget: i64) {
    let mut tx = repo.pool().begin().await.unwrap();
    wave_update_tx(
        &mut tx,
        root,
        WavePatch {
            tree_task_budget: Some(Some(budget)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn declaration(index: usize, key: &str) -> TaskDeclaration {
    TaskDeclaration {
        block_index: Some(index),
        block_id: format!("b_{index:04x}"),
        key: key.into(),
        kind: "codex".into(),
        goal: format!("goal {key}"),
        acceptance: None,
        gate: None,
        no_gate_reason: Some("not needed".into()),
        depends_on: Vec::new(),
        context: json!({}),
        cwd: None,
        priority: 0,
        refs: Vec::new(),
        declared_by: "spec".into(),
        released_by_user: false,
        spawn: "in-wave".into(),
        tombstoned_by: None,
        ready: true,
        tombstone: false,
    }
}

fn declarations(keys: &[&str]) -> Vec<TaskDeclaration> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| declaration(index, key))
        .collect()
}

async fn project(repo: &SqlxRepo, wave: &str, keys: &[&str]) {
    let declarations = declarations(keys);
    let diags = vec![Vec::new(); declarations.len()];
    let mut tx = repo.pool().begin().await.unwrap();
    project_tasks_tx(&mut tx, wave, &declarations, &diags)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Byte-level snapshot of every projected row, the same shape the PR-A
/// rebuild-stability acceptance uses.
async fn task_bytes(repo: &SqlxRepo) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT json_object('id',id,'wave_id',wave_id,'key',key,'kind',kind,'goal',goal, \
         'context',context_json,'acceptance',acceptance_criteria,'cwd',cwd, \
         'depends_on',depends_on_json,'priority',priority,'gate',gate_json,'status',status, \
         'declared_by',declared_by,'origin',origin,'decl_ready',decl_ready, \
         'spawn',spawn,'child_wave_id',child_wave_id) \
         FROM tasks ORDER BY wave_id, key",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap()
}

async fn share_of(repo: &SqlxRepo, wave: &str) -> TreeShare {
    let mut conn = repo.pool().acquire().await.unwrap();
    match wave_tree_term(&mut conn, wave).await.unwrap().term {
        WaveTreeTerm::Share(share) => share,
        other => panic!("expected a share for {wave}, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Single source of truth: the budget column lives only on the root.
// ---------------------------------------------------------------------------

/// POSITIVE assertion on the column, not on "the budget took effect" — with
/// the kernel default equal to the configured value, a behavioral assertion
/// would be vacuous. Every wave-create path (the `child-wave` operation
/// included) goes through `wave_create_tx`.
#[tokio::test]
async fn every_created_wave_lands_a_null_tree_task_budget() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;

    for wave in [&root, &child] {
        let budget: Option<i64> =
            sqlx::query_scalar("SELECT tree_task_budget FROM waves WHERE id=?1")
                .bind(wave)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(budget, None, "wave {wave} must be born without a budget");
    }
    // And the column has no DB DEFAULT that a future INSERT could fall into.
    let default: Option<String> = sqlx::query_scalar(
        "SELECT dflt_value FROM pragma_table_info('waves') WHERE name='tree_task_budget'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(default, None);
}

/// Root-only, enforced by the shared in-tx writer rather than the route, so a
/// direct repository caller cannot slip a second budget onto a child.
#[tokio::test]
async fn tree_task_budget_patch_on_a_child_is_refused_by_the_shared_writer() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;

    let mut tx = repo.pool().begin().await.unwrap();
    let error = wave_update_tx(
        &mut tx,
        &child,
        WavePatch {
            tree_task_budget: Some(Some(4)),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(
        error.to_string().contains("root-only") && error.to_string().contains(&root),
        "{error}"
    );

    let budget: Option<i64> = sqlx::query_scalar("SELECT tree_task_budget FROM waves WHERE id=?1")
        .bind(&child)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(budget, None);

    // The same patch on the root succeeds, and a present-null resets it.
    set_tree_budget(&repo, &root, 4).await;
    let budget: Option<i64> = sqlx::query_scalar("SELECT tree_task_budget FROM waves WHERE id=?1")
        .bind(&root)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(budget, Some(4));
    let mut tx = repo.pool().begin().await.unwrap();
    wave_update_tx(
        &mut tx,
        &root,
        WavePatch {
            tree_task_budget: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let budget: Option<i64> = sqlx::query_scalar("SELECT tree_task_budget FROM waves WHERE id=?1")
        .bind(&root)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(budget, None);

    // The shared writer, not only the REST route, owns the fixed bound that
    // keeps whole-tree reprojection from becoming an unbounded writer hold.
    let mut tx = repo.pool().begin().await.unwrap();
    let error = wave_update_tx(
        &mut tx,
        &root,
        WavePatch {
            tree_task_budget: Some(Some(MAX_TREE_TASK_BUDGET + 1)),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(error.to_string().contains("between 0 and 64"), "{error}");
}

// ---------------------------------------------------------------------------
// The quota split.
// ---------------------------------------------------------------------------

/// `Σ share = B` over a REAL tree, including the non-divisible case where the
/// remainder is handed to a prefix of the `(created_at, id)` order.
#[tokio::test]
async fn shares_over_a_real_tree_sum_to_the_budget() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let a = seed_wave(&repo, &cove, "a").await;
    let b = seed_wave(&repo, &cove, "b").await;
    let c = seed_wave(&repo, &cove, "c").await;
    link(&repo, &a, &root).await;
    link(&repo, &b, &root).await;
    link(&repo, &c, &a).await;
    let members = [&root, &a, &b, &c];
    for (index, wave) in members.iter().enumerate() {
        stamp_created_at(&repo, wave, 1000 + index as i64).await;
    }

    // 7 = 4*1 + 3: the first three waves in creation order get 2, the last 1.
    set_tree_budget(&repo, &root, 7).await;
    let mut total = 0;
    for (index, wave) in members.iter().enumerate() {
        let share = share_of(&repo, wave).await;
        assert_eq!(share.root_id, root);
        assert_eq!(share.budget, 7);
        assert_eq!(share.members, 4);
        assert_eq!(share.share, if index < 3 { 2 } else { 1 }, "wave {index}");
        total += share.share;
    }
    assert_eq!(total, 7);

    // The divisible case, same tree.
    set_tree_budget(&repo, &root, 8).await;
    let total: i64 = {
        let mut sum = 0;
        for wave in members {
            sum += share_of(&repo, wave).await.share;
        }
        sum
    };
    assert_eq!(total, 8);
}

/// The quota order itself is part of the split definition. Insertion order is
/// deliberately the reverse of `created_at`; deleting the ORDER BY must make
/// this fail instead of inheriting SQLite's current scan order by accident.
#[tokio::test]
async fn quota_remainder_follows_created_at_not_insertion_order() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root-first").await;
    let child = seed_wave(&repo, &cove, "child-second").await;
    // Fix ids opposite to created_at order. Without the final ORDER BY,
    // SQLite is free to return the GROUP BY's id order (`a-child` first), so
    // the oracle does not depend on today's query plan or random UUIDs.
    sqlx::query("UPDATE waves SET id='z-root' WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET id='a-child' WHERE id=?1")
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    link(&repo, "a-child", "z-root").await;
    stamp_created_at(&repo, "z-root", 1_000).await;
    stamp_created_at(&repo, "a-child", 2_000).await;
    set_tree_budget(&repo, "z-root", 1).await;

    assert_eq!(share_of(&repo, "z-root").await.share, 1);
    assert_eq!(share_of(&repo, "a-child").await.share, 0);
}

/// Equal-millisecond creation is common. The secondary id key is therefore a
/// correctness input, not decorative SQL.
#[tokio::test]
async fn quota_remainder_breaks_equal_created_at_ties_by_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root-first").await;
    let child = seed_wave(&repo, &cove, "child-second").await;
    sqlx::query("UPDATE waves SET id='z-root' WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET id='a-child' WHERE id=?1")
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    link(&repo, "a-child", "z-root").await;
    stamp_created_at(&repo, "z-root", 1_000).await;
    stamp_created_at(&repo, "a-child", 1_000).await;
    set_tree_budget(&repo, "z-root", 1).await;

    assert_eq!(share_of(&repo, "a-child").await.share, 1);
    assert_eq!(share_of(&repo, "z-root").await.share, 0);
}

/// The share is a function of the tree's SHAPE only. Adding pending rows in a
/// sibling — the projection's own OUTPUT — must not move anybody's share.
/// This is the property that keeps rebuild ≡ incremental true on a tree; a
/// shared sibling count would fail it.
#[tokio::test]
async fn shares_do_not_move_when_siblings_accumulate_pending_rows() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    stamp_created_at(&repo, &root, 1).await;
    stamp_created_at(&repo, &child, 2).await;
    set_tree_budget(&repo, &root, 6).await;

    let before = share_of(&repo, &child).await;
    project(&repo, &root, &["r1", "r2", "r3"]).await;
    let after = share_of(&repo, &child).await;
    assert_eq!(before, after);
    assert_eq!(after.share, 3);
}

/// Two rebuild orders over the same tree produce the same rows, byte for byte.
///
/// The mutation this buys: replace `share` with a shared count that subtracts
/// sibling pending rows. Then whichever wave projects first takes the whole
/// budget and the two orders diverge.
#[tokio::test]
async fn two_rebuild_orders_over_one_tree_agree_byte_for_byte() {
    async fn run(order: [usize; 2]) -> Vec<String> {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = seed_cove(&repo).await;
        let root = seed_wave(&repo, &cove, "root").await;
        let child = seed_wave(&repo, &cove, "child").await;
        link(&repo, &child, &root).await;
        stamp_created_at(&repo, &root, 1).await;
        stamp_created_at(&repo, &child, 2).await;
        set_tree_budget(&repo, &root, 4).await;
        // Both waves want more than their share of 2.
        let waves = [root, child];
        let keys: [&[&str]; 2] = [&["a1", "a2", "a3"], &["b1", "b2", "b3"]];
        for index in order {
            project(&repo, &waves[index], keys[index]).await;
        }
        // Re-project both in the same order: a rebuild is idempotent.
        for index in order {
            project(&repo, &waves[index], keys[index]).await;
        }
        let mut rows: Vec<String> = task_bytes(&repo)
            .await
            .into_iter()
            // Row ids and wave ids are random per run; compare the projection
            // shape (which key landed where) rather than the identifiers.
            .map(|row| {
                let mut value: serde_json::Value = serde_json::from_str(&row).unwrap();
                let object = value.as_object_mut().unwrap();
                object.remove("id");
                object.remove("wave_id");
                value.to_string()
            })
            .collect();
        // Wave ids are random per run, so the SQL order is not comparable
        // across runs; the KEY identifies which wave admitted the row.
        rows.sort();
        rows
    }

    let forward = run([0, 1]).await;
    let backward = run([1, 0]).await;
    assert_eq!(forward.len(), 4, "each wave admits exactly its share of 2");
    assert_eq!(forward, backward);
}

/// Projecting the same document twice changes nothing — no row churn, no
/// second round of kernel events. The tree term must not read its own output.
#[tokio::test]
async fn projecting_the_same_document_twice_inside_a_tree_is_identical() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    set_tree_budget(&repo, &root, 4).await;

    let keys = ["k1", "k2", "k3"];
    let decls = declarations(&keys);
    let diags = vec![Vec::new(); decls.len()];

    let mut tx = repo.pool().begin().await.unwrap();
    let first = project_tasks_tx(&mut tx, &root, &decls, &diags)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after_first = task_bytes(&repo).await;

    let mut tx = repo.pool().begin().await.unwrap();
    let second = project_tasks_tx(&mut tx, &root, &decls, &diags)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after_second = task_bytes(&repo).await;

    assert_eq!(after_first, after_second);
    assert!(!first.changed_keys.is_empty());
    assert!(
        second.changed_keys.is_empty(),
        "second projection changed {:?}",
        second.changed_keys
    );
    assert!(second.kernel_events.is_empty());
    assert_eq!(after_second.len(), 2, "share of 2 admits two of three keys");
}

/// The rejected declaration is attributed to the TREE, naming the root wave —
/// not to this wave's own ceiling, which is not what stopped it.
#[tokio::test]
async fn over_share_declarations_are_diagnosed_against_the_root_wave() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    set_ceiling(&repo, &child, 32).await;
    set_tree_budget(&repo, &root, 2).await;

    let decls = declarations(&["k1", "k2"]);
    let diags = vec![Vec::new(); decls.len()];
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &child, &decls, &diags, false)
        .await
        .unwrap();

    assert!(verdicts[0].schedulable, "the first key fits the share of 1");
    assert!(!verdicts[1].schedulable);
    let diagnostic = verdicts[1]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        .expect("tree_budget_exhausted");
    assert_eq!(diagnostic.related_wave_id.as_deref(), Some(root.as_str()));
    assert_eq!(
        diagnostic
            .message_args
            .get("root_wave_id")
            .and_then(|value| value.as_str()),
        Some(root.as_str())
    );
    assert_eq!(diagnostic.action.as_deref(), Some("raise_tree_task_budget"));
    let sentence = &diagnostic.message;
    assert!(
        sentence.contains(&root),
        "sentence must name the root: {sentence}"
    );
    assert!(sentence.contains("tree_task_budget"), "{sentence}");
    assert!(
        sentence.contains("in-flight task in this wave"),
        "{sentence}"
    );
    assert!(!sentence.contains("elsewhere in the tree"), "{sentence}");
    // The wave's own ceiling is NOT the binding constraint here, so the
    // ceiling diagnostic must not be what the reader sees.
    assert!(
        !verdicts[1]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "spec_task_ceiling")
    );
}

#[tokio::test]
async fn zero_share_diagnostic_explains_the_shape_and_effective_actions() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    stamp_created_at(&repo, &root, 1).await;
    stamp_created_at(&repo, &child, 2).await;
    set_tree_budget(&repo, &root, 1).await;

    let decls = declarations(&["k1"]);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &child, &decls, &[vec![]], false)
        .await
        .unwrap();
    let diagnostic = verdicts[0]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        .expect("zero share has a tree diagnostic");
    assert!(diagnostic.message.contains("zero task share"));
    assert!(diagnostic.message.contains("remove extra child waves"));
    assert!(!diagnostic.message.contains("finish"));
}

/// When the wave's own ceiling is the tighter bound, the existing ceiling
/// diagnostic still wins — the tree code does not swallow it.
#[tokio::test]
async fn a_tighter_wave_ceiling_still_reports_the_ceiling_diagnostic() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    set_ceiling(&repo, &child, 1).await;
    set_tree_budget(&repo, &root, 32).await;

    let decls = declarations(&["k1", "k2"]);
    let diags = vec![Vec::new(); decls.len()];
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &child, &decls, &diags, false)
        .await
        .unwrap();
    assert!(
        verdicts[1]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "spec_task_ceiling")
    );
}

/// Equal numeric limits are still a tree constraint: raising only this wave's
/// ceiling cannot admit another task while its deterministic share stays 32.
#[tokio::test]
async fn an_equal_tree_share_reports_the_tree_knob() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    set_ceiling(&repo, &child, 32).await;
    set_tree_budget(&repo, &root, 64).await;

    let keys = (0..33)
        .map(|index| format!("k{index:02}"))
        .collect::<Vec<_>>();
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let decls = declarations(&key_refs);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(
        &mut conn,
        &child,
        &decls,
        &vec![Vec::new(); decls.len()],
        false,
    )
    .await
    .unwrap();
    let diagnostic = verdicts[32]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        .expect("equal share/ceiling must name the tree constraint");
    assert_eq!(diagnostic.action.as_deref(), Some("raise_tree_task_budget"));
    assert!(
        !verdicts[32]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "spec_task_ceiling")
    );
}

// ---------------------------------------------------------------------------
// Fail-closed root resolution and the non-tree short circuit.
// ---------------------------------------------------------------------------

/// A wave whose root cannot be resolved gets NOTHING scheduled. "No resolvable
/// tree ⇒ skip the tree term" would leave the whole subtree unbounded, which is
/// the single outcome the tree budget exists to prevent.
#[tokio::test]
async fn unresolvable_root_fails_closed_for_every_declaration() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let a = seed_wave(&repo, &cove, "a").await;
    let b = seed_wave(&repo, &cove, "b").await;
    // A 2-cycle: neither wave has a NULL-parent ancestor.
    link(&repo, &a, &b).await;
    link(&repo, &b, &a).await;

    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        wave_tree_term(&mut conn, &a).await.unwrap().term,
        WaveTreeTerm::RootUnresolved
    );

    let decls = declarations(&["k1", "k2"]);
    let diags = vec![Vec::new(); decls.len()];
    let verdicts = evaluate_schedulability(&mut conn, &a, &decls, &diags, false)
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 2);
    for verdict in &verdicts {
        assert!(!verdict.schedulable);
        assert!(
            verdict
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "tree_root_unresolved")
        );
    }

    // And the write path materializes nothing.
    project(&repo, &a, &["k1", "k2"]).await;
    assert!(task_bytes(&repo).await.is_empty());
}

/// Root failure closes tree admission without skipping the independent §6.5
/// withdrawal/read-state path.
#[tokio::test]
async fn unresolved_root_preserves_withdrawal_and_deleted_block_read_verdicts() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "root").await;
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &root).await;
    project(&repo, &child, &["k1"]).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1 AND key='k1'")
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    link(&repo, &root, &child).await;

    let mut withdrawn = declaration(0, "k1");
    withdrawn.ready = false;
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &child, &[withdrawn], &[vec![]], false)
        .await
        .unwrap();
    assert_eq!(verdicts[0].withdrawal, Some(super::WithdrawalEdge::Ready));
    assert!(
        verdicts[0]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "tree_root_unresolved")
    );

    let deleted = evaluate_schedulability(&mut conn, &child, &[], &[], true)
        .await
        .unwrap();
    let verdict = deleted
        .iter()
        .find(|verdict| verdict.key == "k1")
        .expect("deleted in-flight block remains readable");
    assert_eq!(verdict.status.as_deref(), Some("running"));
    assert!(
        verdict
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "context_stale_declaration")
    );
}

/// A chain deeper than the legal bound is also unresolvable, not "rooted at
/// whatever the truncated walk happened to reach".
#[tokio::test]
async fn an_over_deep_chain_fails_closed() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let mut chain = Vec::new();
    for index in 0..=(MAX_WAVE_TREE_DEPTH + 2) {
        chain.push(seed_wave(&repo, &cove, &format!("w{index}")).await);
    }
    for index in 1..chain.len() {
        link(&repo, &chain[index], &chain[index - 1]).await;
    }
    let deepest = chain.last().unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        wave_tree_term(&mut conn, deepest).await.unwrap().term,
        WaveTreeTerm::RootUnresolved
    );
    assert_eq!(
        wave_tree_term(&mut conn, &chain[0]).await.unwrap().term,
        WaveTreeTerm::RootUnresolved,
        "the root must reject a member set containing an over-deep node"
    );
}

/// The countable seam for the non-tree short circuit: a wave with no parent
/// and no child runs ZERO recursive tree statements. Asserting "a non-tree
/// wave behaves byte-identically" would be vacuous; this can fail.
#[tokio::test]
async fn a_non_tree_wave_runs_zero_recursive_tree_queries() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let lonely = seed_wave(&repo, &cove, "lonely").await;
    let mut conn = repo.pool().acquire().await.unwrap();

    let outcome = wave_tree_term(&mut conn, &lonely).await.unwrap();
    assert_eq!(outcome.term, WaveTreeTerm::NotInTree);
    assert_eq!(outcome.tree_cte_queries, 0);

    // Give it a child and the walks must actually run — otherwise the
    // zero-count assertion above could be satisfied by never walking at all.
    let child = seed_wave(&repo, &cove, "child").await;
    link(&repo, &child, &lonely).await;
    let outcome = wave_tree_term(&mut conn, &lonely).await.unwrap();
    assert!(outcome.tree_cte_queries > 0);
    assert!(matches!(outcome.term, WaveTreeTerm::Share(_)));
}

/// A binding singleton budget is still N=1 and share=B. This explicit B=1 is
/// below the wave ceiling, so the provably-non-binding shortcut is illegal.
#[tokio::test]
async fn an_explicit_budget_applies_to_a_singleton_root() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let lonely = seed_wave(&repo, &cove, "lonely").await;
    set_ceiling(&repo, &lonely, 3).await;
    set_tree_budget(&repo, &lonely, 1).await;
    let decls = declarations(&["k1", "k2", "k3"]);
    let diags = vec![Vec::new(); decls.len()];
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &lonely, &decls, &diags, false)
        .await
        .unwrap();
    assert!(verdicts[0].schedulable);
    assert!(verdicts[1..].iter().all(|verdict| !verdict.schedulable));
    let outcome = wave_tree_term(&mut conn, &lonely).await.unwrap();
    assert_eq!(outcome.tree_cte_queries, 0);
    assert!(matches!(
        outcome.term,
        WaveTreeTerm::Share(TreeShare {
            members: 1,
            share: 1,
            ..
        })
    ));
}

/// Both sides of the singleton shortcut comparison use the same nullable
/// limit decoder. A present-null ceiling means the kernel default (32), not
/// zero; with B=1 the tree term therefore remains binding.
#[tokio::test]
async fn a_null_ceiling_and_tiny_budget_still_bind_a_singleton_root() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let lonely = seed_wave(&repo, &cove, "lonely").await;
    let mut tx = repo.pool().begin().await.unwrap();
    wave_update_tx(
        &mut tx,
        &lonely,
        WavePatch {
            spec_task_ceiling: Some(None),
            tree_task_budget: Some(Some(1)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let decls = declarations(&["k1", "k2", "k3", "k4", "k5"]);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(
        &mut conn,
        &lonely,
        &decls,
        &vec![Vec::new(); decls.len()],
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        verdicts
            .iter()
            .filter(|verdict| verdict.schedulable)
            .count(),
        1
    );
    assert!(matches!(
        wave_tree_term(&mut conn, &lonely).await.unwrap().term,
        WaveTreeTerm::Share(TreeShare {
            budget: 1,
            members: 1,
            share: 1,
            ..
        })
    ));
}

/// PATCH back to NULL restores the kernel default; it does not remove the
/// bound. Both enforcement points must therefore read B=32 for this wave.
#[tokio::test]
async fn resetting_an_explicit_budget_to_null_keeps_the_default_bound() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let lonely = seed_wave(&repo, &cove, "lonely").await;
    set_ceiling(&repo, &lonely, 40).await;
    set_tree_budget(&repo, &lonely, 40).await;
    let mut tx = repo.pool().begin().await.unwrap();
    wave_update_tx(
        &mut tx,
        &lonely,
        WavePatch {
            tree_task_budget: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let keys = (0..40)
        .map(|index| format!("k{index:02}"))
        .collect::<Vec<_>>();
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let decls = declarations(&key_refs);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(
        &mut conn,
        &lonely,
        &decls,
        &vec![Vec::new(); decls.len()],
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        verdicts
            .iter()
            .filter(|verdict| verdict.schedulable)
            .count(),
        32
    );
    assert!(verdicts[32..].iter().all(|verdict| {
        verdict
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "tree_budget_exhausted")
    }));
    assert!(matches!(
        wave_tree_term(&mut conn, &lonely).await.unwrap().term,
        WaveTreeTerm::Share(TreeShare {
            budget: 32,
            members: 1,
            share: 32,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// The downward CTE's termination guard.
// ---------------------------------------------------------------------------

/// A downward 2-cycle terminates fast. Deleting `WHERE down.depth <= ?2` from
/// the descendant CTE hangs this test instead of failing it — which is exactly
/// why the static gate in `wave_tree.rs` exists alongside it.
#[tokio::test]
async fn a_downward_two_cycle_terminates_quickly() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let a = seed_wave(&repo, &cove, "a").await;
    let b = seed_wave(&repo, &cove, "b").await;
    link(&repo, &a, &b).await;
    link(&repo, &b, &a).await;

    let started = Instant::now();
    let mut conn = repo.pool().acquire().await.unwrap();
    // Enumerate members starting AT the cycle, bypassing root resolution so
    // the descendant walk itself is what has to terminate.
    let members: Vec<(String, i64)> = sqlx::query_as(super::wave_tree::WAVE_TREE_MEMBERS_SQL)
        .bind(&a)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    let inventory = super::wave_tree::wave_tree_spec_inventory(&mut conn, &a)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(inventory, 0);
    assert!(members.iter().any(|(id, _)| id == &a));
}

#[test]
fn share_helper_matches_the_documented_formula() {
    // floor(B/N) with the remainder on a prefix of the order.
    assert_eq!(deterministic_share(7, 4, 0), 2);
    assert_eq!(deterministic_share(7, 4, 3), 1);
    assert_eq!(deterministic_share(0, 4, 0), 0);
    assert_eq!(deterministic_share(2, 5, 4), 0);
}
