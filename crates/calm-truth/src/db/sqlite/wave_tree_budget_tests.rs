//! #985 slice 6 PR-B — acceptance for the tree-level budget: the
//! deterministic quota split in `evaluate_schedulability`, the fail-closed
//! root resolution, bounded singleton evaluation, and the downward CTE's
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
    wave_tree_spec_inventory, wave_tree_term,
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

async fn live_spec_count(repo: &SqlxRepo, root: &str) -> i64 {
    let mut conn = repo.pool().acquire().await.unwrap();
    wave_tree_spec_inventory(&mut conn, root).await.unwrap()
}

async fn mark_all_tasks_as_running(repo: &SqlxRepo, wave: &str) {
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1")
        .bind(wave)
        .execute(repo.pool())
        .await
        .unwrap();
}

/// Byte-level snapshot of every projected row, the same shape the PR-A
/// rebuild-stability acceptance uses.
async fn task_bytes(repo: &SqlxRepo) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT json_object('id',id,'wave_id',wave_id,'key',key,'kind',kind,'goal',goal, \
         'context',context_json,'acceptance',acceptance_criteria,'cwd',cwd, \
         'depends_on',depends_on_json,'priority',priority,'gate',gate_json,'status',status, \
         'declared_by',declared_by,'decl_ready',decl_ready, \
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
        sentence.contains("tree's excess in-flight work"),
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

/// Operability property for capacity diagnostics over a bounded input space.
///
/// Exhaust `N=1..=3`, `B=0..=6`, every target member, `ceiling=0..=5`, and
/// target-member block-in-flight occupancy of either zero or three rows. The
/// occupancy axis crosses all three documented local relations:
/// ceiling above, equal to, and below immutable in-flight occupancy. This is
/// the smallest dense grid that crosses two remainder boundaries for every
/// supported member count and includes both local and tree self-overage.
///
/// The bounded grid deliberately excludes exactly two families, each covered
/// by a named acceptance below: sibling overage (tree-wide freeze) and the
/// production maximum (`B=64`) no-solution boundary. Exhaustive dimensions
/// must be derived from the design's declared state set; any excluded family
/// must be listed here with its independent acceptance, rather than selected
/// from states the implementation happens to produce.
///
/// Perform every capacity action named on each rejection, using the minimum
/// tree target carried by the diagnostic, then the SAME member/report must
/// admit more declarations. The assertion is deliberately on the effect, not
/// on which code or prose happens to describe it.
#[tokio::test]
async fn the_diagnosed_capacity_action_increases_admission() {
    async fn project_and_capacity_diagnostics(
        repo: &SqlxRepo,
        wave: &str,
        keys: &[&str],
    ) -> (i64, Vec<calm_types::report_blocks::tasks::Diagnostic>) {
        let decls = declarations(keys);
        let mut tx = repo.pool().begin().await.unwrap();
        let outcome = project_tasks_tx(&mut tx, wave, &decls, &vec![Vec::new(); decls.len()])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let admitted: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1")
            .bind(wave)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        let diagnostics = outcome
            .diagnostics
            .into_iter()
            .flat_map(|verdict| verdict.diagnostics)
            .filter(|diagnostic| {
                matches!(
                    diagnostic.code.as_str(),
                    "spec_task_ceiling" | "tree_budget_exhausted"
                )
            })
            .collect();
        (admitted, diagnostics)
    }

    async fn seed_block_inflight(repo: &SqlxRepo, wave: &str, count: usize, prefix: &str) {
        if count == 0 {
            return;
        }
        let keys = (0..count)
            .map(|index| format!("{prefix}-{index}"))
            .collect::<Vec<_>>();
        let refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
        project(repo, wave, &refs).await;
        sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1")
            .bind(wave)
            .execute(repo.pool())
            .await
            .unwrap();
    }

    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let keys = (0..10)
        .map(|index| format!("candidate-{index:02}"))
        .collect::<Vec<_>>();
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let mut checked = 0usize;
    let mut ineffective = Vec::new();

    for members in 1..=3usize {
        for budget in 0..=6i64 {
            for target_index in 0..members {
                for block_inflight in [0usize, 3] {
                    let case = format!(
                        "N={members},B={budget},target={target_index},block_inflight={block_inflight}"
                    );
                    let root = seed_wave(&repo, &cove, &format!("{case} root")).await;
                    let mut waves = vec![root.clone()];
                    for index in 1..members {
                        let child = seed_wave(&repo, &cove, &format!("{case} child {index}")).await;
                        link(&repo, &child, &root).await;
                        waves.push(child);
                    }
                    for (index, wave) in waves.iter().enumerate() {
                        stamp_created_at(&repo, wave, index as i64 + 1).await;
                        set_ceiling(&repo, wave, MAX_TREE_TASK_BUDGET).await;
                    }
                    set_tree_budget(&repo, &root, MAX_TREE_TASK_BUDGET).await;
                    seed_block_inflight(
                        &repo,
                        &waves[target_index],
                        block_inflight,
                        "block-inflight",
                    )
                    .await;

                    for ceiling in 0..=5i64 {
                        checked += 1;
                        sqlx::query("DELETE FROM tasks WHERE wave_id=?1 AND status='pending'")
                            .bind(&waves[target_index])
                            .execute(repo.pool())
                            .await
                            .unwrap();
                        sqlx::query("UPDATE waves SET tree_task_budget=?1 WHERE id=?2")
                            .bind(budget)
                            .bind(&root)
                            .execute(repo.pool())
                            .await
                            .unwrap();
                        set_ceiling(&repo, &waves[target_index], ceiling).await;

                        let (before, diagnostics) = project_and_capacity_diagnostics(
                            &repo,
                            &waves[target_index],
                            &key_refs,
                        )
                        .await;
                        assert!(!diagnostics.is_empty(), "{case},C={ceiling}: no rejection");
                        let actions = diagnostics
                            .iter()
                            .filter_map(|diagnostic| {
                                diagnostic.action.as_ref().map(|action| {
                                    (
                                        action.clone(),
                                        (
                                            diagnostic
                                                .message_args
                                                .get("minimum_tree_task_budget")
                                                .and_then(serde_json::Value::as_i64),
                                            diagnostic
                                                .message_args
                                                .get("minimum_spec_task_ceiling")
                                                .and_then(serde_json::Value::as_i64),
                                        ),
                                    )
                                })
                            })
                            .collect::<std::collections::BTreeMap<_, _>>();
                        assert!(
                            !actions.is_empty(),
                            "{case},C={ceiling}: no capacity action"
                        );
                        for (action, (minimum_tree_budget, minimum_ceiling)) in &actions {
                            match action.as_str() {
                                "raise_spec_task_ceiling" => {
                                    let minimum = minimum_ceiling.expect(
                                        "ceiling action must carry an occupancy-safe minimum",
                                    );
                                    set_ceiling(&repo, &waves[target_index], minimum).await;
                                }
                                "raise_tree_task_budget" => {
                                    let minimum = minimum_tree_budget.expect(
                                        "tree action must carry a remainder-safe minimum budget",
                                    );
                                    set_tree_budget(&repo, &root, minimum).await;
                                }
                                other => {
                                    panic!("capacity diagnostic named unsupported action {other}")
                                }
                            }
                        }
                        let (after, _) = project_and_capacity_diagnostics(
                            &repo,
                            &waves[target_index],
                            &key_refs,
                        )
                        .await;
                        if after <= before {
                            ineffective.push(format!(
                                "{case},C={ceiling}: following {actions:?} did not increase admission: {before} -> {after}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 504, "bounded capacity grid drifted");
    assert!(
        ineffective.is_empty(),
        "{} bounded capacity actions were ineffective; first failures: {:#?}",
        ineffective.len(),
        &ineffective[..ineffective.len().min(12)]
    );
}

/// At the product maximum there may be no legal B that gives the target one
/// more slot. The diagnostic must not advertise an impossible PATCH, whether
/// the tree is ordinarily full or frozen by immutable in-flight occupancy.
#[tokio::test]
async fn an_unreachable_tree_budget_target_reports_no_raise_action() {
    async fn rejected_tree_diagnostic(
        repo: &SqlxRepo,
        wave: &str,
    ) -> calm_types::report_blocks::tasks::Diagnostic {
        let decls = declarations(&["new-task"]);
        let mut conn = repo.pool().acquire().await.unwrap();
        evaluate_schedulability(&mut conn, wave, &decls, &[vec![]], false)
            .await
            .unwrap()
            .into_iter()
            .flat_map(|verdict| verdict.diagnostics)
            .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
            .expect("tree rejection")
    }

    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;

    let full = seed_wave(&repo, &cove, "max-budget-full").await;
    set_ceiling(&repo, &full, MAX_TREE_TASK_BUDGET + 1).await;
    set_tree_budget(&repo, &full, MAX_TREE_TASK_BUDGET).await;
    let full_keys = (0..MAX_TREE_TASK_BUDGET)
        .map(|index| format!("full-{index:02}"))
        .collect::<Vec<_>>();
    let full_refs = full_keys.iter().map(String::as_str).collect::<Vec<_>>();
    project(&repo, &full, &full_refs).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1")
        .bind(&full)
        .execute(repo.pool())
        .await
        .unwrap();
    let diagnostic = rejected_tree_diagnostic(&repo, &full).await;
    assert_eq!(diagnostic.action, None);
    assert!(
        !diagnostic
            .message_args
            .contains_key("minimum_tree_task_budget")
    );
    assert!(diagnostic.message.contains("cannot be released by raising"));
    assert!(!diagnostic.message.contains("at least 0"));

    let frozen = seed_wave(&repo, &cove, "max-budget-frozen").await;
    set_ceiling(&repo, &frozen, MAX_TREE_TASK_BUDGET).await;
    set_tree_budget(&repo, &frozen, MAX_TREE_TASK_BUDGET).await;
    let in_flight_keys = (0..33)
        .map(|index| format!("in-flight-{index:02}"))
        .collect::<Vec<_>>();
    let in_flight_refs = in_flight_keys
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    project(&repo, &frozen, &in_flight_refs).await;
    mark_all_tasks_as_running(&repo, &frozen).await;
    let child = seed_wave(&repo, &cove, "max-budget-frozen-child").await;
    link(&repo, &child, &frozen).await;
    stamp_created_at(&repo, &frozen, 1).await;
    stamp_created_at(&repo, &child, 2).await;
    let diagnostic = rejected_tree_diagnostic(&repo, &frozen).await;
    assert_eq!(diagnostic.action, None);
    assert_eq!(
        diagnostic
            .message_args
            .get("admission_frozen")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(
        !diagnostic
            .message_args
            .contains_key("minimum_tree_task_budget")
    );
    assert!(diagnostic.message.contains("cannot be released by raising"));
    assert!(
        diagnostic
            .message
            .contains("reduce the number of tree members")
    );

    set_ceiling(&repo, &frozen, 0).await;
    let decls = declarations(&["still-frozen"]);
    let mut conn = repo.pool().acquire().await.unwrap();
    let diagnostics = evaluate_schedulability(&mut conn, &frozen, &decls, &[vec![]], false)
        .await
        .unwrap()
        .into_iter()
        .flat_map(|verdict| verdict.diagnostics)
        .filter(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "spec_task_ceiling" | "tree_budget_exhausted"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "both impossible bounds must be named");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.action.is_none())
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message.contains("current configuration"))
    );
}

/// The exhaustive grid owns all documented local occupancy relations. This
/// focused wiring case also pins the exact recovery targets and server prose
/// when the configured ceiling is below nonzero in-flight occupancy.
#[tokio::test]
async fn a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "occupied-local-bound").await;
    set_ceiling(&repo, &root, MAX_TREE_TASK_BUDGET).await;
    set_tree_budget(&repo, &root, MAX_TREE_TASK_BUDGET).await;
    project(&repo, &root, &["live-a", "live-b", "live-c"]).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET tree_task_budget=2,spec_task_ceiling=1 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    let decls = declarations(&["next"]);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &root, &decls, &[vec![]], false)
        .await
        .unwrap();
    let diagnostics = &verdicts[0].diagnostics;
    let ceiling = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "spec_task_ceiling")
        .expect("ceiling diagnostic");
    assert_eq!(ceiling.action.as_deref(), Some("raise_spec_task_ceiling"));
    assert_eq!(
        ceiling
            .message_args
            .get("minimum_spec_task_ceiling")
            .and_then(serde_json::Value::as_i64),
        Some(4)
    );
    assert!(
        ceiling.message.contains("to at least 4"),
        "{}",
        ceiling.message
    );
    assert_eq!(
        ceiling
            .message_args
            .get("admission_frozen")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_ne!(
        ceiling
            .message_args
            .get("bounds_tied")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "a tree-wide freeze is not a local/tree capacity tie"
    );
    let tree = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        .expect("tree diagnostic");
    assert_eq!(tree.action.as_deref(), Some("raise_tree_task_budget"));
    assert_eq!(
        tree.message_args
            .get("minimum_tree_task_budget")
            .and_then(serde_json::Value::as_i64),
        Some(4)
    );
    assert!(tree.message.contains("at least 4"), "{}", tree.message);
    drop(conn);

    set_ceiling(&repo, &root, 4).await;
    set_tree_budget(&repo, &root, 4).await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &root, &decls, &[vec![]], false)
        .await
        .unwrap();
    assert!(
        verdicts[0].schedulable,
        "following both actions must add one slot"
    );
}

// ---------------------------------------------------------------------------
// Fail-closed root resolution and bounded singleton evaluation.
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

/// A binding singleton budget is still N=1 and share=B.
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
    assert_eq!(outcome.tree_cte_queries, 2);
    assert!(matches!(
        outcome.term,
        WaveTreeTerm::Share(TreeShare {
            members: 1,
            share: 1,
            ..
        })
    ));
}

/// A present-null ceiling means the kernel default (32), not zero; with B=1
/// the tree term therefore remains binding.
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

/// Degradation rule for D.4 #7. Every non-terminal spec row consumes tree
/// share. Starting above B therefore admits no new row; capacity returns only
/// as the in-flight rows terminate, and projection never edits those rows.
#[tokio::test]
async fn in_flight_spec_consumes_tree_share_until_it_terminates() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "upgraded-root").await;
    set_ceiling(&repo, &root, 8).await;
    set_tree_budget(&repo, &root, 3).await;
    project(&repo, &root, &["in-flight-a", "in-flight-b", "in-flight-c"]).await;
    mark_all_tasks_as_running(&repo, &root).await;
    // This is the degraded state under test: K=3 pre-existing in-flight rows
    // meet a newly effective B=2.
    sqlx::query("UPDATE waves SET tree_task_budget=2 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    let in_flight_bytes = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT json_group_array(json_object('id',id,'status',status, \
             'goal',goal,'context',context_json,'updated',updated_at_ms)) \
             FROM tasks WHERE wave_id=?1 AND status='running' ORDER BY key",
        )
        .bind(&root)
        .fetch_one(repo.pool())
        .await
        .unwrap()
    };
    let live_count = || async {
        let mut conn = repo.pool().acquire().await.unwrap();
        wave_tree_spec_inventory(&mut conn, &root).await.unwrap()
    };

    let before = in_flight_bytes().await;
    project(&repo, &root, &["new-a", "new-b"]).await;
    assert_eq!(
        in_flight_bytes().await,
        before,
        "report writes must not edit in-flight rows"
    );
    assert_eq!(live_count().await, 3);
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1 AND key LIKE 'new-%'")
            .bind(&root)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(new_rows, 0, "K >= B must force new-block capacity to zero");

    sqlx::query("UPDATE tasks SET status='done' WHERE wave_id=?1 AND key='in-flight-a'")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    project(&repo, &root, &["new-a", "new-b"]).await;
    assert_eq!(live_count().await, 2, "K == B still has zero new capacity");

    sqlx::query("UPDATE tasks SET status='done' WHERE wave_id=?1 AND key='in-flight-b'")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    project(&repo, &root, &["new-a", "new-b"]).await;
    assert_eq!(
        live_count().await,
        2,
        "one terminated in-flight row restores one slot"
    );

    sqlx::query("UPDATE tasks SET status='done' WHERE wave_id=?1 AND key='in-flight-c'")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    project(&repo, &root, &["new-a", "new-b"]).await;
    assert_eq!(
        live_count().await,
        2,
        "all in-flight termination restores the full B=2"
    );
}

/// r6 B1/codex construction: a default-budget singleton with two existing
/// live rows must not stack 31 new block rows on top and commit 33 > B=32.
#[tokio::test]
async fn singleton_default_budget_counts_in_flight_occupancy_before_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "upgraded-default-root").await;
    set_ceiling(&repo, &root, 32).await;
    project(&repo, &root, &["in-flight-a", "in-flight-b"]).await;
    mark_all_tasks_as_running(&repo, &root).await;

    let keys = (0..31)
        .map(|index| format!("new-{index:02}"))
        .collect::<Vec<_>>();
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    project(&repo, &root, &key_refs).await;

    assert_eq!(live_spec_count(&repo, &root).await, 32);
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1 AND key LIKE 'new-%'")
            .bind(&root)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(new_rows, 30, "in-flight occupancy must consume two of B=32");
}

/// r6 B1/subagent construction: B=6, ceiling=8 and four existing live rows
/// leave exactly two slots; the report must not admit all four new keys.
#[tokio::test]
async fn singleton_explicit_budget_counts_in_flight_occupancy_before_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "upgraded-explicit-root").await;
    set_ceiling(&repo, &root, 8).await;
    set_tree_budget(&repo, &root, 8).await;
    project(
        &repo,
        &root,
        &["in-flight-a", "in-flight-b", "in-flight-c", "in-flight-d"],
    )
    .await;
    mark_all_tasks_as_running(&repo, &root).await;
    set_tree_budget(&repo, &root, 6).await;

    project(&repo, &root, &["new-a", "new-b", "new-c", "new-d"]).await;

    assert_eq!(live_spec_count(&repo, &root).await, 6);
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1 AND key LIKE 'new-%'")
            .bind(&root)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(new_rows, 2, "in-flight occupancy must leave only B-K slots");
}

/// The overage freeze is tree-wide, not merely local to the member carrying
/// excess in-flight rows. With K=B but root fixed occupancy 5 > share 4, the
/// child must not use its otherwise-free fourth slot and push Σ to 9.
#[tokio::test]
async fn in_flight_member_overage_freezes_new_blocks_across_the_tree() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let root = seed_wave(&repo, &cove, "upgrade-root").await;
    let child = seed_wave(&repo, &cove, "upgrade-child").await;
    link(&repo, &child, &root).await;
    stamp_created_at(&repo, &root, 1).await;
    stamp_created_at(&repo, &child, 2).await;
    set_ceiling(&repo, &root, 8).await;
    set_ceiling(&repo, &child, 8).await;
    set_tree_budget(&repo, &root, 16).await;
    project(
        &repo,
        &root,
        &["root-a", "root-b", "root-c", "root-d", "root-e"],
    )
    .await;
    project(&repo, &child, &["child-a", "child-b", "child-c"]).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id IN (?1,?2)")
        .bind(&root)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET tree_task_budget=8 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    let new_declarations = declarations(&["new-a", "new-b"]);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(
        &mut conn,
        &child,
        &new_declarations,
        &vec![Vec::new(); new_declarations.len()],
        false,
    )
    .await
    .unwrap();
    assert!(
        verdicts.iter().take(new_declarations.len()).all(|verdict| {
            !verdict.schedulable
                && verdict
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        }),
        "{verdicts:#?}"
    );
    for diagnostic in verdicts
        .iter()
        .flat_map(|verdict| &verdict.diagnostics)
        .filter(|diagnostic| diagnostic.code == "tree_budget_exhausted")
    {
        assert!(
            diagnostic.message.contains("tree's excess in-flight work"),
            "a sibling overage must not tell the reader to wait for this wave: {}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("is frozen because"),
            "the diagnostic must state the actual tree-wide freeze: {}",
            diagnostic.message
        );
        assert!(
            !diagnostic.message.contains("slice") && !diagnostic.message.contains("used up"),
            "a target with unused share must not be described as full: {}",
            diagnostic.message
        );
        assert!(!diagnostic.message.contains("task in this wave"));
    }
    drop(conn);

    // A sibling's overage freezes the tree even though this target has unused
    // share. A zero local ceiling is another binding setting, but not a tie:
    // its copy must name the freeze without claiming this wave's share is full.
    set_ceiling(&repo, &child, 0).await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let frozen_at_zero = evaluate_schedulability(
        &mut conn,
        &child,
        &declarations(&["frozen-at-zero"]),
        &[vec![]],
        false,
    )
    .await
    .unwrap();
    let ceiling = frozen_at_zero[0]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "spec_task_ceiling")
        .expect("the frozen tree and zero local ceiling both bind");
    assert_eq!(
        ceiling
            .message_args
            .get("admission_frozen")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_ne!(
        ceiling
            .message_args
            .get("bounds_tied")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(ceiling.message.contains("is frozen"), "{}", ceiling.message);
    assert!(
        !ceiling.message.contains("tree share are both reached"),
        "unused target share must not be reported as full: {}",
        ceiling.message
    );
    drop(conn);
    set_ceiling(&repo, &child, 8).await;

    sqlx::query("UPDATE waves SET tree_task_budget=4 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    let tighter = evaluate_schedulability(
        &mut conn,
        &child,
        &new_declarations,
        &vec![Vec::new(); new_declarations.len()],
        false,
    )
    .await
    .unwrap();
    let tree_diagnostic = tighter[0]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        .expect("tree diagnostic");
    let minimum = tree_diagnostic
        .message_args
        .get("minimum_tree_task_budget")
        .and_then(serde_json::Value::as_i64);
    assert!(
        tree_diagnostic.message.contains("at least 9"),
        "the server copy must carry the executable minimum: {}",
        tree_diagnostic.message
    );
    drop(conn);
    assert_eq!(minimum, Some(9), "every sibling overage must fit too");
    set_tree_budget(&repo, &root, minimum.unwrap()).await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let raised = evaluate_schedulability(
        &mut conn,
        &child,
        &new_declarations,
        &vec![Vec::new(); new_declarations.len()],
        false,
    )
    .await
    .unwrap();
    assert!(
        raised[0].schedulable,
        "the diagnosed sibling-freeze minimum must increase admission"
    );
    drop(conn);
    sqlx::query("UPDATE waves SET tree_task_budget=8 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    project(&repo, &child, &["new-a", "new-b"]).await;
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1 AND key LIKE 'new-%'")
            .bind(&child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        new_rows, 0,
        "one over-share member must freeze every member"
    );
    assert_eq!(wave_tree_spec_inventory(&mut conn, &root).await.unwrap(), 8);
    drop(conn);

    sqlx::query("UPDATE tasks SET status='done' WHERE wave_id=?1 AND key='root-a'")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    project(&repo, &child, &["new-a", "new-b"]).await;
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1 AND key LIKE 'new-%'")
            .bind(&child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        new_rows, 1,
        "capacity returns once every member fits its share"
    );
    assert_eq!(wave_tree_spec_inventory(&mut conn, &root).await.unwrap(), 8);
}

/// Equal creation timestamps deliberately fall through to the persisted id
/// order. When the child id sorts first it receives B=9's remainder, leaving
/// the root's five in-flight rows over its share until B reaches 10.
#[tokio::test]
async fn equal_created_at_with_child_id_first_requires_ten_to_unfreeze() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo).await;
    let first = seed_wave(&repo, &cove, "equal-time-first").await;
    let second = seed_wave(&repo, &cove, "equal-time-second").await;
    let (child, root) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    link(&repo, &child, &root).await;
    stamp_created_at(&repo, &root, 1).await;
    stamp_created_at(&repo, &child, 1).await;
    assert!(child < root, "the fixture must put the child id first");

    set_ceiling(&repo, &root, 8).await;
    set_ceiling(&repo, &child, 8).await;
    set_tree_budget(&repo, &root, 16).await;
    project(
        &repo,
        &root,
        &["root-a", "root-b", "root-c", "root-d", "root-e"],
    )
    .await;
    project(&repo, &child, &["child-a", "child-b", "child-c"]).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE wave_id IN (?1,?2)")
        .bind(&root)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET tree_task_budget=4 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    let new_declarations = declarations(&["new-a"]);
    let mut conn = repo.pool().acquire().await.unwrap();
    let verdicts = evaluate_schedulability(&mut conn, &child, &new_declarations, &[vec![]], false)
        .await
        .unwrap();
    let tree_diagnostic = verdicts[0]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
        .expect("tree diagnostic");
    let minimum = tree_diagnostic
        .message_args
        .get("minimum_tree_task_budget")
        .and_then(serde_json::Value::as_i64);
    assert!(
        tree_diagnostic.message.contains("at least 10"),
        "the server copy must carry the id-ordered executable minimum: {}",
        tree_diagnostic.message
    );
    drop(conn);
    assert_eq!(minimum, Some(10), "the root must also fit its five rows");

    set_tree_budget(&repo, &root, minimum.unwrap()).await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let raised = evaluate_schedulability(&mut conn, &child, &new_declarations, &[vec![]], false)
        .await
        .unwrap();
    assert!(
        raised[0].schedulable,
        "the id-ordered sibling-freeze minimum must increase admission"
    );
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
