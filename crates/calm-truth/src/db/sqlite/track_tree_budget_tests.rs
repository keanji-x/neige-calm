//! Acceptance for the tree-level budget, driven through the production functions.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use calm_types::report_blocks::tasks::TaskDeclaration;
use serde_json::json;

use super::track_tree::{
    MAX_TRACK_TREE_DEPTH, MAX_TREE_TASK_BUDGET, TrackTreeTerm, TreeShare, deterministic_share,
    track_tree_planner_inventory, track_tree_term,
};
use super::{
    SqlxRepo, evaluate_schedulability, project_tasks_tx, track_create_tx, track_update_tx,
};
use crate::model::{NewArea, NewTrack, RequestTheme, TrackPatch};

use super::area_create_tx;

async fn seed_area(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "tree".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    area.id.to_string()
}

async fn seed_track(repo: &SqlxRepo, area_id: &str, title: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area_id.to_string().into(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    track.id.to_string()
}

async fn link(repo: &SqlxRepo, child: &str, parent: &str) {
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(parent)
        .bind(child)
        .execute(repo.pool())
        .await
        .unwrap();
}

/// Tracks minted inside one millisecond would tie-break on the random id,
/// making the EXPECTED order unknowable to the test.
async fn stamp_created_at(repo: &SqlxRepo, track: &str, created_at: i64) {
    sqlx::query("UPDATE tracks SET created_at=?1 WHERE id=?2")
        .bind(created_at)
        .bind(track)
        .execute(repo.pool())
        .await
        .unwrap();
}

async fn set_ceiling(repo: &SqlxRepo, track: &str, ceiling: i64) {
    sqlx::query("UPDATE tracks SET planner_task_ceiling=?1 WHERE id=?2")
        .bind(ceiling)
        .bind(track)
        .execute(repo.pool())
        .await
        .unwrap();
}

async fn set_tree_budget(repo: &SqlxRepo, root: &str, budget: i64) {
    let mut tx = repo.pool().begin().await.unwrap();
    track_update_tx(
        &mut tx,
        root,
        TrackPatch {
            tree_task_budget: Some(Some(budget)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn declaration(index: usize, key: &str) -> TaskDeclaration {
    use calm_types::report_blocks::tasks::{PLANNER_DECLARATION_AUTHOR, project_task_declarations};
    use calm_types::report_blocks::{KIND_PROSE, KIND_TASK};
    use calm_types::track_report::ReportBlock;
    let mut blocks: Vec<ReportBlock> = (0..index)
        .map(|position| ReportBlock {
            id: format!("b_{position:04x}"),
            kind: KIND_PROSE.into(),
            rev: 0,
            payload: json!({"markdown":"preceding prose"}),
        })
        .collect();
    blocks.push(ReportBlock {
        id: format!("b_{index:04x}"),
        kind: KIND_TASK.into(),
        rev: 0,
        payload: json!({"key":key,"kind":"codex","goal":format!("goal {key}"),
            "no_gate_reason":"not needed","declared_by":PLANNER_DECLARATION_AUTHOR,"ready":true}),
    });
    let (mut declarations, diagnostics) = project_task_declarations(&blocks);
    assert!(diagnostics.iter().all(Vec::is_empty));
    declarations.remove(0)
}

fn declarations(keys: &[&str]) -> Vec<TaskDeclaration> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| declaration(index, key))
        .collect()
}

async fn project(repo: &SqlxRepo, track: &str, keys: &[&str]) {
    let declarations = declarations(keys);
    let diags = vec![Vec::new(); declarations.len()];
    let mut tx = repo.pool().begin().await.unwrap();
    project_tasks_tx(&mut tx, track, &declarations, &diags)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn live_planner_count(repo: &SqlxRepo, root: &str) -> i64 {
    let mut conn = repo.pool().acquire().await.unwrap();
    track_tree_planner_inventory(&mut conn, root).await.unwrap()
}

async fn mark_all_tasks_as_running(repo: &SqlxRepo, track: &str) {
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1")
        .bind(track)
        .execute(repo.pool())
        .await
        .unwrap();
}

async fn task_bytes(repo: &SqlxRepo) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT json_object('id',id,'track_id',track_id,'key',key,'kind',kind,'goal',goal, \
         'context',context_json,'acceptance',acceptance_criteria,'cwd',cwd, \
         'depends_on',depends_on_json,'priority',priority,'gate',gate_json,'status',status, \
         'declared_by',declared_by,'decl_ready',decl_ready, \
         'spawn',spawn,'child_track_id',child_track_id) \
         FROM tasks ORDER BY track_id, key",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap()
}

async fn share_of(repo: &SqlxRepo, track: &str) -> TreeShare {
    let mut conn = repo.pool().acquire().await.unwrap();
    match track_tree_term(&mut conn, track).await.unwrap().term {
        TrackTreeTerm::Share(share) => share,
        other => panic!("expected a share for {track}, got {other:?}"),
    }
}

/// Assert on the column, not on "the budget took effect": the kernel default
/// equals the configured value, so a behavioral assertion would be vacuous.
#[tokio::test]
async fn every_created_track_lands_a_null_tree_task_budget() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
    link(&repo, &child, &root).await;

    for track in [&root, &child] {
        let budget: Option<i64> =
            sqlx::query_scalar("SELECT tree_task_budget FROM tracks WHERE id=?1")
                .bind(track)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(budget, None, "track {track} must be born without a budget");
    }
    let default: Option<String> = sqlx::query_scalar(
        "SELECT dflt_value FROM pragma_table_info('tracks') WHERE name='tree_task_budget'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(default, None);
}

#[tokio::test]
async fn tree_task_budget_patch_on_a_child_is_refused_by_the_shared_writer() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
    link(&repo, &child, &root).await;

    let mut tx = repo.pool().begin().await.unwrap();
    let error = track_update_tx(
        &mut tx,
        &child,
        TrackPatch {
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

    let budget: Option<i64> = sqlx::query_scalar("SELECT tree_task_budget FROM tracks WHERE id=?1")
        .bind(&child)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(budget, None);

    set_tree_budget(&repo, &root, 4).await;
    let budget: Option<i64> = sqlx::query_scalar("SELECT tree_task_budget FROM tracks WHERE id=?1")
        .bind(&root)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(budget, Some(4));
    let mut tx = repo.pool().begin().await.unwrap();
    track_update_tx(
        &mut tx,
        &root,
        TrackPatch {
            tree_task_budget: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let budget: Option<i64> = sqlx::query_scalar("SELECT tree_task_budget FROM tracks WHERE id=?1")
        .bind(&root)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(budget, None);

    let mut tx = repo.pool().begin().await.unwrap();
    let error = track_update_tx(
        &mut tx,
        &root,
        TrackPatch {
            tree_task_budget: Some(Some(MAX_TREE_TASK_BUDGET + 1)),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(error.to_string().contains("between 0 and 64"), "{error}");
}

#[tokio::test]
async fn shares_over_a_real_tree_sum_to_the_budget() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let a = seed_track(&repo, &area, "a").await;
    let b = seed_track(&repo, &area, "b").await;
    let c = seed_track(&repo, &area, "c").await;
    link(&repo, &a, &root).await;
    link(&repo, &b, &root).await;
    link(&repo, &c, &a).await;
    let members = [&root, &a, &b, &c];
    for (index, track) in members.iter().enumerate() {
        stamp_created_at(&repo, track, 1000 + index as i64).await;
    }

    // 7 = 4*1 + 3: the first three tracks in creation order get 2, the last 1.
    set_tree_budget(&repo, &root, 7).await;
    let mut total = 0;
    for (index, track) in members.iter().enumerate() {
        let share = share_of(&repo, track).await;
        assert_eq!(share.root_id, root);
        assert_eq!(share.budget, 7);
        assert_eq!(share.members, 4);
        assert_eq!(share.share, if index < 3 { 2 } else { 1 }, "track {index}");
        total += share.share;
    }
    assert_eq!(total, 7);

    set_tree_budget(&repo, &root, 8).await;
    let total: i64 = {
        let mut sum = 0;
        for track in members {
            sum += share_of(&repo, track).await.share;
        }
        sum
    };
    assert_eq!(total, 8);
}

/// Insertion order is deliberately the reverse of `created_at`.
#[tokio::test]
async fn quota_remainder_follows_created_at_not_insertion_order() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root-first").await;
    let child = seed_track(&repo, &area, "child-second").await;
    // Fix ids opposite to created_at order so the oracle does not depend on
    // today's query plan or random UUIDs.
    sqlx::query("UPDATE tracks SET id='z-root' WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET id='a-child' WHERE id=?1")
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

#[tokio::test]
async fn quota_remainder_breaks_equal_created_at_ties_by_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root-first").await;
    let child = seed_track(&repo, &area, "child-second").await;
    sqlx::query("UPDATE tracks SET id='z-root' WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET id='a-child' WHERE id=?1")
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

#[tokio::test]
async fn shares_do_not_move_when_siblings_accumulate_pending_rows() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
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

#[tokio::test]
async fn two_rebuild_orders_over_one_tree_agree_byte_for_byte() {
    async fn run(order: [usize; 2]) -> Vec<String> {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = seed_area(&repo).await;
        let root = seed_track(&repo, &area, "root").await;
        let child = seed_track(&repo, &area, "child").await;
        link(&repo, &child, &root).await;
        stamp_created_at(&repo, &root, 1).await;
        stamp_created_at(&repo, &child, 2).await;
        set_tree_budget(&repo, &root, 4).await;
        let tracks = [root, child];
        let keys: [&[&str]; 2] = [&["a1", "a2", "a3"], &["b1", "b2", "b3"]];
        for index in order {
            project(&repo, &tracks[index], keys[index]).await;
        }
        for index in order {
            project(&repo, &tracks[index], keys[index]).await;
        }
        let mut rows: Vec<String> = task_bytes(&repo)
            .await
            .into_iter()
            // Row ids and track ids are random per run; compare the projection shape.
            .map(|row| {
                let mut value: serde_json::Value = serde_json::from_str(&row).unwrap();
                let object = value.as_object_mut().unwrap();
                object.remove("id");
                object.remove("track_id");
                value.to_string()
            })
            .collect();
        rows.sort();
        rows
    }

    let forward = run([0, 1]).await;
    let backward = run([1, 0]).await;
    assert_eq!(forward.len(), 4, "each track admits exactly its share of 2");
    assert_eq!(forward, backward);
}

#[tokio::test]
async fn projecting_the_same_document_twice_inside_a_tree_is_identical() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
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

#[tokio::test]
async fn over_share_declarations_are_diagnosed_against_the_root_track() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
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
    assert_eq!(diagnostic.related_track_id.as_deref(), Some(root.as_str()));
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
    assert!(
        !verdicts[1]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "planner_task_ceiling")
    );
}

#[tokio::test]
async fn zero_share_diagnostic_explains_the_shape_and_effective_actions() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
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
    assert!(diagnostic.message.contains("remove extra child tracks"));
    assert!(!diagnostic.message.contains("finish"));
}

#[tokio::test]
async fn a_tighter_track_ceiling_still_reports_the_ceiling_diagnostic() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
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
            .any(|diagnostic| diagnostic.code == "planner_task_ceiling")
    );
}

/// Exhaust `N=1..=3`, `B=0..=6`, every target member, `ceiling=0..=5`, and
/// target occupancy of zero or three rows. Sibling overage and the `B=64`
/// no-solution boundary are excluded here and covered by named cases below.
#[tokio::test]
async fn the_diagnosed_capacity_action_increases_admission() {
    async fn project_and_capacity_diagnostics(
        repo: &SqlxRepo,
        track: &str,
        keys: &[&str],
    ) -> (i64, Vec<calm_types::report_blocks::tasks::Diagnostic>) {
        let decls = declarations(keys);
        let mut tx = repo.pool().begin().await.unwrap();
        let outcome = project_tasks_tx(&mut tx, track, &decls, &vec![Vec::new(); decls.len()])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let admitted: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1")
            .bind(track)
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
                    "planner_task_ceiling" | "tree_budget_exhausted"
                )
            })
            .collect();
        (admitted, diagnostics)
    }

    async fn seed_block_inflight(repo: &SqlxRepo, track: &str, count: usize, prefix: &str) {
        if count == 0 {
            return;
        }
        let keys = (0..count)
            .map(|index| format!("{prefix}-{index}"))
            .collect::<Vec<_>>();
        let refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
        project(repo, track, &refs).await;
        sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1")
            .bind(track)
            .execute(repo.pool())
            .await
            .unwrap();
    }

    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
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
                    let root = seed_track(&repo, &area, &format!("{case} root")).await;
                    let mut tracks = vec![root.clone()];
                    for index in 1..members {
                        let child =
                            seed_track(&repo, &area, &format!("{case} child {index}")).await;
                        link(&repo, &child, &root).await;
                        tracks.push(child);
                    }
                    for (index, track) in tracks.iter().enumerate() {
                        stamp_created_at(&repo, track, index as i64 + 1).await;
                        set_ceiling(&repo, track, MAX_TREE_TASK_BUDGET).await;
                    }
                    set_tree_budget(&repo, &root, MAX_TREE_TASK_BUDGET).await;
                    seed_block_inflight(
                        &repo,
                        &tracks[target_index],
                        block_inflight,
                        "block-inflight",
                    )
                    .await;

                    for ceiling in 0..=5i64 {
                        checked += 1;
                        sqlx::query("DELETE FROM tasks WHERE track_id=?1 AND status='pending'")
                            .bind(&tracks[target_index])
                            .execute(repo.pool())
                            .await
                            .unwrap();
                        sqlx::query("UPDATE tracks SET tree_task_budget=?1 WHERE id=?2")
                            .bind(budget)
                            .bind(&root)
                            .execute(repo.pool())
                            .await
                            .unwrap();
                        set_ceiling(&repo, &tracks[target_index], ceiling).await;

                        let (before, diagnostics) = project_and_capacity_diagnostics(
                            &repo,
                            &tracks[target_index],
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
                                                .get("minimum_planner_task_ceiling")
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
                                "raise_planner_task_ceiling" => {
                                    let minimum = minimum_ceiling.expect(
                                        "ceiling action must carry an occupancy-safe minimum",
                                    );
                                    set_ceiling(&repo, &tracks[target_index], minimum).await;
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
                            &tracks[target_index],
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

#[tokio::test]
async fn an_unreachable_tree_budget_target_reports_no_raise_action() {
    async fn rejected_tree_diagnostic(
        repo: &SqlxRepo,
        track: &str,
    ) -> calm_types::report_blocks::tasks::Diagnostic {
        let decls = declarations(&["new-task"]);
        let mut conn = repo.pool().acquire().await.unwrap();
        evaluate_schedulability(&mut conn, track, &decls, &[vec![]], false)
            .await
            .unwrap()
            .into_iter()
            .flat_map(|verdict| verdict.diagnostics)
            .find(|diagnostic| diagnostic.code == "tree_budget_exhausted")
            .expect("tree rejection")
    }

    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;

    let full = seed_track(&repo, &area, "max-budget-full").await;
    set_ceiling(&repo, &full, MAX_TREE_TASK_BUDGET + 1).await;
    set_tree_budget(&repo, &full, MAX_TREE_TASK_BUDGET).await;
    let full_keys = (0..MAX_TREE_TASK_BUDGET)
        .map(|index| format!("full-{index:02}"))
        .collect::<Vec<_>>();
    let full_refs = full_keys.iter().map(String::as_str).collect::<Vec<_>>();
    project(&repo, &full, &full_refs).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1")
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

    let frozen = seed_track(&repo, &area, "max-budget-frozen").await;
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
    let child = seed_track(&repo, &area, "max-budget-frozen-child").await;
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
                "planner_task_ceiling" | "tree_budget_exhausted"
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

#[tokio::test]
async fn a_frozen_track_with_nonzero_ceiling_occupancy_names_both_bounds() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "occupied-local-bound").await;
    set_ceiling(&repo, &root, MAX_TREE_TASK_BUDGET).await;
    set_tree_budget(&repo, &root, MAX_TREE_TASK_BUDGET).await;
    project(&repo, &root, &["live-a", "live-b", "live-c"]).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET tree_task_budget=2,planner_task_ceiling=1 WHERE id=?1")
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
        .find(|diagnostic| diagnostic.code == "planner_task_ceiling")
        .expect("ceiling diagnostic");
    assert_eq!(
        ceiling.action.as_deref(),
        Some("raise_planner_task_ceiling")
    );
    assert_eq!(
        ceiling
            .message_args
            .get("minimum_planner_task_ceiling")
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

#[tokio::test]
async fn unresolvable_root_fails_closed_for_every_declaration() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let a = seed_track(&repo, &area, "a").await;
    let b = seed_track(&repo, &area, "b").await;
    link(&repo, &a, &b).await;
    link(&repo, &b, &a).await;

    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        track_tree_term(&mut conn, &a).await.unwrap().term,
        TrackTreeTerm::RootUnresolved
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

    project(&repo, &a, &["k1", "k2"]).await;
    assert!(task_bytes(&repo).await.is_empty());
}

#[tokio::test]
async fn unresolved_root_preserves_withdrawal_and_deleted_block_read_verdicts() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
    link(&repo, &child, &root).await;
    project(&repo, &child, &["k1"]).await;
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='k1'")
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

#[tokio::test]
async fn an_over_deep_chain_fails_closed() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let mut chain = Vec::new();
    for index in 0..=(MAX_TRACK_TREE_DEPTH + 2) {
        chain.push(seed_track(&repo, &area, &format!("w{index}")).await);
    }
    for index in 1..chain.len() {
        link(&repo, &chain[index], &chain[index - 1]).await;
    }
    let deepest = chain.last().unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        track_tree_term(&mut conn, deepest).await.unwrap().term,
        TrackTreeTerm::RootUnresolved
    );
    assert_eq!(
        track_tree_term(&mut conn, &chain[0]).await.unwrap().term,
        TrackTreeTerm::RootUnresolved,
        "the root must reject a member set containing an over-deep node"
    );
}

#[tokio::test]
async fn an_explicit_budget_applies_to_a_singleton_root() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let lonely = seed_track(&repo, &area, "lonely").await;
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
    let outcome = track_tree_term(&mut conn, &lonely).await.unwrap();
    assert_eq!(outcome.tree_cte_queries, 2);
    assert!(matches!(
        outcome.term,
        TrackTreeTerm::Share(TreeShare {
            members: 1,
            share: 1,
            ..
        })
    ));
}

/// A present-null ceiling means the kernel default (32), not zero.
#[tokio::test]
async fn a_null_ceiling_and_tiny_budget_still_bind_a_singleton_root() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let lonely = seed_track(&repo, &area, "lonely").await;
    let mut tx = repo.pool().begin().await.unwrap();
    track_update_tx(
        &mut tx,
        &lonely,
        TrackPatch {
            planner_task_ceiling: Some(None),
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
        track_tree_term(&mut conn, &lonely).await.unwrap().term,
        TrackTreeTerm::Share(TreeShare {
            budget: 1,
            members: 1,
            share: 1,
            ..
        })
    ));
}

/// Raw SQL builds the overage: the production tree-budget PATCH rejects this
/// input atomically and cannot commit the degraded state.
#[tokio::test]
async fn raw_sql_tree_overage_consumes_share_until_inflight_terminates() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "upgraded-root").await;
    set_ceiling(&repo, &root, 8).await;
    set_tree_budget(&repo, &root, 3).await;
    project(&repo, &root, &["in-flight-a", "in-flight-b", "in-flight-c"]).await;
    mark_all_tasks_as_running(&repo, &root).await;
    sqlx::query("UPDATE tracks SET tree_task_budget=2 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    let in_flight_bytes = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT json_group_array(json_object('id',id,'status',status, \
             'goal',goal,'context',context_json,'updated',updated_at_ms)) \
             FROM tasks WHERE track_id=?1 AND status='running' ORDER BY key",
        )
        .bind(&root)
        .fetch_one(repo.pool())
        .await
        .unwrap()
    };
    let live_count = || async {
        let mut conn = repo.pool().acquire().await.unwrap();
        track_tree_planner_inventory(&mut conn, &root)
            .await
            .unwrap()
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
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1 AND key LIKE 'new-%'")
            .bind(&root)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(new_rows, 0, "K >= B must force new-block capacity to zero");

    sqlx::query("UPDATE tasks SET status='done' WHERE track_id=?1 AND key='in-flight-a'")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    project(&repo, &root, &["new-a", "new-b"]).await;
    assert_eq!(live_count().await, 2, "K == B still has zero new capacity");

    sqlx::query("UPDATE tasks SET status='done' WHERE track_id=?1 AND key='in-flight-b'")
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

    sqlx::query("UPDATE tasks SET status='done' WHERE track_id=?1 AND key='in-flight-c'")
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

#[tokio::test]
async fn singleton_default_budget_counts_in_flight_occupancy_before_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "upgraded-default-root").await;
    set_ceiling(&repo, &root, 64).await;
    project(&repo, &root, &["in-flight-a", "in-flight-b"]).await;
    mark_all_tasks_as_running(&repo, &root).await;

    let keys = (0..31)
        .map(|index| format!("new-{index:02}"))
        .collect::<Vec<_>>();
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    project(&repo, &root, &key_refs).await;

    assert_eq!(live_planner_count(&repo, &root).await, 32);
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1 AND key LIKE 'new-%'")
            .bind(&root)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(new_rows, 30, "in-flight occupancy must consume two of B=32");
}

#[tokio::test]
async fn singleton_explicit_budget_counts_in_flight_occupancy_before_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "upgraded-explicit-root").await;
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

    assert_eq!(live_planner_count(&repo, &root).await, 6);
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1 AND key LIKE 'new-%'")
            .bind(&root)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(new_rows, 2, "in-flight occupancy must leave only B-K slots");
}

#[tokio::test]
async fn in_flight_member_overage_freezes_new_blocks_across_the_tree() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "upgrade-root").await;
    let child = seed_track(&repo, &area, "upgrade-child").await;
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
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id IN (?1,?2)")
        .bind(&root)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET tree_task_budget=8 WHERE id=?1")
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
    assert_eq!(
        verdicts.len(),
        new_declarations.len() + 3,
        "all three undeclared in-flight rows must contribute synthetic verdicts: {verdicts:#?}"
    );
    let synthetic_keys = verdicts
        .iter()
        .skip(new_declarations.len())
        .map(|verdict| verdict.key.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        synthetic_keys,
        BTreeSet::from(["child-a", "child-b", "child-c"]),
        "synthetic verdicts must cover the complete undeclared in-flight set"
    );
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
    assert!(
        verdicts.iter().skip(new_declarations.len()).all(|verdict| {
            !verdict.schedulable
                && verdict
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("cannot be withdrawn"))
        }),
        "extra verdicts must describe synthetic withdrawals: {verdicts:#?}"
    );
    for diagnostic in verdicts
        .iter()
        .flat_map(|verdict| &verdict.diagnostics)
        .filter(|diagnostic| diagnostic.code == "tree_budget_exhausted")
    {
        assert!(
            diagnostic.message.contains("tree's excess in-flight work"),
            "a sibling overage must not tell the reader to wait for this track: {}",
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
        assert!(!diagnostic.message.contains("task in this track"));
    }
    drop(conn);

    // A zero local ceiling is another binding setting, but not a tie: its copy
    // must name the freeze without claiming this track's share is full.
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
        .find(|diagnostic| diagnostic.code == "planner_task_ceiling")
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

    sqlx::query("UPDATE tracks SET tree_task_budget=4 WHERE id=?1")
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
    sqlx::query("UPDATE tracks SET tree_task_budget=8 WHERE id=?1")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    project(&repo, &child, &["new-a", "new-b"]).await;
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1 AND key LIKE 'new-%'")
            .bind(&child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        new_rows, 0,
        "one over-share member must freeze every member"
    );
    assert_eq!(
        track_tree_planner_inventory(&mut conn, &root)
            .await
            .unwrap(),
        8
    );
    drop(conn);

    sqlx::query("UPDATE tasks SET status='done' WHERE track_id=?1 AND key='root-a'")
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    project(&repo, &child, &["new-a", "new-b"]).await;
    let new_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1 AND key LIKE 'new-%'")
            .bind(&child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        new_rows, 1,
        "capacity returns once every member fits its share"
    );
    assert_eq!(
        track_tree_planner_inventory(&mut conn, &root)
            .await
            .unwrap(),
        8
    );
}

#[tokio::test]
async fn equal_created_at_with_child_id_first_requires_ten_to_unfreeze() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let first = seed_track(&repo, &area, "equal-time-first").await;
    let second = seed_track(&repo, &area, "equal-time-second").await;
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
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id IN (?1,?2)")
        .bind(&root)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET tree_task_budget=4 WHERE id=?1")
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

#[tokio::test]
async fn resetting_an_explicit_budget_to_null_keeps_the_default_bound() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let lonely = seed_track(&repo, &area, "lonely").await;
    set_ceiling(&repo, &lonely, 40).await;
    set_tree_budget(&repo, &lonely, 40).await;
    let mut tx = repo.pool().begin().await.unwrap();
    track_update_tx(
        &mut tx,
        &lonely,
        TrackPatch {
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
        track_tree_term(&mut conn, &lonely).await.unwrap().term,
        TrackTreeTerm::Share(TreeShare {
            budget: 32,
            members: 1,
            share: 32,
            ..
        })
    ));
}

/// Deleting `WHERE down.depth <= ?2` from the descendant CTE hangs this test
/// instead of failing it.
#[tokio::test]
async fn a_downward_two_cycle_terminates_quickly() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let a = seed_track(&repo, &area, "a").await;
    let b = seed_track(&repo, &area, "b").await;
    link(&repo, &a, &b).await;
    link(&repo, &b, &a).await;

    let started = Instant::now();
    let mut conn = repo.pool().acquire().await.unwrap();
    // Start AT the cycle, bypassing root resolution, so the descendant walk itself
    // is what has to terminate.
    let members: Vec<(String, i64)> = sqlx::query_as(super::track_tree::TRACK_TREE_MEMBERS_SQL)
        .bind(&a)
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    let inventory = super::track_tree::track_tree_planner_inventory(&mut conn, &a)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(inventory, 0);
    assert!(members.iter().any(|(id, _)| id == &a));
}

#[test]
fn share_helper_matches_the_documented_formula() {
    assert_eq!(deterministic_share(7, 4, 0), 2);
    assert_eq!(deterministic_share(7, 4, 3), 1);
    assert_eq!(deterministic_share(0, 4, 0), 0);
    assert_eq!(deterministic_share(2, 5, 4), 0);
}
