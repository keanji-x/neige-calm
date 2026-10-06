//! #2087 B0: the data migration that respells stored tool names with `_` as the only separator.
//! Rows are seeded under the names production stores at migration 133, the real chain from 0134
//! up to the #2087 migration runs, and the result is read back through the real readers: the
//! stored transcript rows and the recipe repository for the revision bump. (The activity projector
//! read `neige_user_notify` rows until #2209 retired the tool; the migrated row is checked as data.)

use calm_server::model::CardRole;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_types::model::NewTrackRecipe;
use serde_json::json;

use super::neige_tool_name_migration::tool_of;
use super::track_activity_fixture::{Fx, fx};

/// The stored `$.item.tool` after the whole chain, written out independently of the migrations.
const RESPELLED: &[(&str, &str)] = &[
    ("calm.track.cat", "neige_track_cat"),
    ("calm.report.write_markdown", "neige_report_write"),
    ("calm.plan.list", "neige_plan_list"),
    ("calm.terminal.observe", "neige_terminal_observe"),
    ("calm.task.complete", "neige_task_done"),
    ("calm.task_completed", "neige_task_done"),
    ("calm.task.fail", "neige_task_fail"),
    ("calm.task_failed", "neige_task_fail"),
    ("neige.task.report_success", "neige_task_done"),
    ("neige.task.report_failure", "neige_task_fail"),
    // History-only names keep their words; only the separator changes.
    ("calm.review.round", "neige_review_round"),
    ("calm.report.blocks.upsert", "neige_report_upsert"),
    ("calm.track.publish", "neige_dev_publish"),
    // A plugin name changes only its prefix until its id and tool are respelled (#2087 B5).
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_pr_checks",
        "plugin_dev.neige.git-forge_gh.pr.checks",
    ),
    (
        "plugin.dev-neige-market_market.quote",
        "plugin_dev-neige-market_market.quote",
    ),
];

/// Tool fields no migration rewrites: a provider tool, an unknown qualified name, an already
/// respelled name.
const KEPT: &[&str] = &["Read", "mcp__calm__plugin_dev_x_unknown", "neige_track_ls"];

/// Every migration from 0134 through version `last`, in order; `expected` names them, so a drifted
/// chain is red. 0136 only creates tables and an index, 0138 only adds a column, 0139 only creates
/// a table, which the fixture's schema already has, and 0140 only renames template ids; none holds
/// a tool name. 0145 and 0147 only add columns (0147 also an index) and 0146 only creates tables,
/// likewise.
pub(super) async fn run_the_chain_through(f: &Fx, last: i64, expected: &[&str]) {
    let chain: Vec<_> = calm_truth::MIGRATOR
        .iter()
        .filter(|m| {
            (134..=last).contains(&m.version)
                && !matches!(m.version, 136 | 138 | 139 | 140 | 145 | 146 | 147)
        })
        .collect();
    assert_eq!(
        chain.iter().map(|m| &*m.description).collect::<Vec<_>>(),
        expected,
        "the chain this test replays drifted"
    );
    for migration in chain {
        sqlx::raw_sql(&migration.sql)
            .execute(&f.pool)
            .await
            .unwrap_or_else(|e| panic!("apply {}: {e}", migration.description));
    }
}

/// The 0134 → 0141 chain this file's expectations are written against.
pub(super) const SEPARATOR_CHAIN: &[&str] = &[
    "neige tool names",
    "worker report recipe names",
    "neige dev publish",
    "tool name separator",
];

/// Applies the one embedded migration named `description` again.
pub(super) async fn rerun_migration(f: &Fx, description: &str) {
    let migration = calm_truth::MIGRATOR
        .iter()
        .find(|m| m.description == description)
        .unwrap_or_else(|| panic!("migration `{description}` is embedded"));
    sqlx::raw_sql(&migration.sql)
        .execute(&f.pool)
        .await
        .unwrap_or_else(|e| panic!("apply {description}: {e}"));
}

#[tokio::test]
async fn stored_tool_names_and_recipes_read_back_with_underscores() {
    let f = fx().await;
    let t = f.track("separator").await;
    let planner = f
        .card(&t, "separator-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "separator-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("separator-thread"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;

    let mut seeded = Vec::new();
    for (i, tool) in RESPELLED
        .iter()
        .map(|(old, _)| *old)
        .chain(KEPT.iter().copied())
        .enumerate()
    {
        // A dotted name elsewhere in params proves only `$.item.tool` is rewritten.
        let params = json!({"item": {"id": format!("call-{i}"), "type": "mcpToolCall",
            "server": "calm", "tool": tool, "status": "completed",
            "arguments": {"call": {"tool": "calm.plan.list"}, "text": "neige.track.cat"}}});
        let id = f
            .transcript_item(
                &ws,
                &planner,
                &t,
                &format!("call-{i}"),
                "mcpToolCall",
                "item/completed",
                params.clone(),
            )
            .await;
        seeded.push((id, tool, params));
    }
    let notify = f
        .transcript_item(
            &ws,
            &planner,
            &t,
            "call-notify",
            "mcpToolCall",
            "item/completed",
            json!({"item": {"id": "call-notify", "type": "mcpToolCall", "server": "calm",
            "tool": "calm.user.notify", "status": "completed",
            "arguments": {"text": "Ship it?"}}}),
        )
        .await;

    let old_body = "Read with calm.calendar.list, capture with calm.source.capture, then \
                    calm.report.commit.\nReport: calm.task.complete; fail: calm.task.fail...\n\
                    CLI: neige task-completed --attempt-id a; neige task-failed --reason why\n\
                    Kept: my_calm.track.cat calm.track.cat.md neige.track.catalog \
                    dev.neige.git-forge neige.kv.set\nEnd: calm.track.ls";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "SPY".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "plain".into(),
            body: "Uses neige.kv.get and neige_report_read only; calm.db stays.".into(),
        })
        .await
        .unwrap();

    run_the_chain_through(&f, 141, SEPARATOR_CHAIN).await;

    for (id, old, mut params) in seeded {
        let expected = RESPELLED
            .iter()
            .find(|(from, _)| *from == old)
            .map_or(old, |(_, to)| to);
        params["item"]["tool"] = json!(expected);
        assert_eq!(tool_of(&f, id).await, params, "{old}");
    }
    assert_eq!(
        tool_of(&f, notify).await["item"]["tool"],
        json!("neige_user_notify"),
        "the chain respells the notify row; since #2209 no projection reads it"
    );
    assert!(
        f.recompute(&t).await.items.is_empty(),
        "#2209: a notify call is no ask"
    );

    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        migrated.body,
        "Read with neige_calendar_list, capture with neige_source_capture, then \
         neige_report_commit.\nReport: neige_task_done; fail: neige_task_fail...\n\
         CLI: neige task done --attempt-id a; neige task fail --reason why\n\
         Kept: my_neige.track.cat neige.track.cat.md neige.track.catalog \
         dev.neige.git-forge neige.kv.set\nEnd: neige_track_ls"
    );
    assert!(migrated.revision > recipe.revision, "{migrated:?}");
    assert!(migrated.updated_at > recipe.updated_at, "{migrated:?}");
    let kept = f
        .repo_dyn
        .track_recipe_get(&untouched.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        (kept.body, kept.revision, kept.updated_at),
        (untouched.body, untouched.revision, untouched.updated_at),
        "a recipe without a dotted kernel name is not touched"
    );

    // Reapplying the migration changes nothing more.
    rerun_migration(&f, "tool name separator").await;
    let repeated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        (repeated.body, repeated.revision, repeated.updated_at),
        (migrated.body, migrated.revision, migrated.updated_at)
    );
}

/// The separator migration alone bumps a recipe's revision once and respells the current
/// dotted names, including the Worker report CLI spelling 0135 wrote.
#[tokio::test]
async fn the_separator_migration_bumps_a_recipe_revision_once() {
    let f = fx().await;
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "current".into(),
            body: "Use neige.report.read, then neige.task.report_success \
                   (`neige task report-success --attempt-id a`) or neige task report-failure."
                .into(),
        })
        .await
        .unwrap();
    rerun_migration(&f, "tool name separator").await;
    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        migrated.body,
        "Use neige_report_read, then neige_task_done \
         (`neige task done --attempt-id a`) or neige task fail."
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
    assert!(migrated.updated_at > recipe.updated_at);
}
