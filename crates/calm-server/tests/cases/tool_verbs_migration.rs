//! #2087 B1: the data migration that renames the view tools to their Unix/git verbs. Rows are
//! seeded under the oldest names production can store (migration 133's `calm.` names; the
//! unreleased workspace tools under their first `neige.` names), the real chain from 0134 through
//! the B1 migration runs, and the result is read back: each transcript row's params (the fe history
//! matches `$.item.tool` by name) and each recipe through the recipe repository.

use calm_server::model::CardRole;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_types::model::NewTrackRecipe;
use serde_json::json;

use super::neige_tool_name_migration::tool_of;
use super::tool_name_separator_migration::{
    SEPARATOR_CHAIN, rerun_migration, run_the_chain_through,
};
use super::track_activity_fixture::{Fx, fx};

const MIGRATION: &str = "tool verbs";

/// The stored `$.item.tool` after the whole chain, written out independently of the migrations.
const RENAMED: &[(&str, &str)] = &[
    ("calm.plan.list", "neige_task_ls"),
    ("calm.plan.cancel", "neige_task_cancel"),
    ("calm.source.list", "neige_source_ls"),
    ("calm.area.outline", "neige_area_ls"),
    ("calm.report.links.backlinks", "neige_link_ls"),
    ("calm.report.blocks.kinds", "neige_report_describe"),
    ("calm.track.state", "neige_track_status"),
    ("calm.get_track_state", "neige_track_status"),
    ("neige.workspace.reports", "neige_workspace_ls"),
    ("neige.workspace.report", "neige_workspace_cat"),
    ("neige.workspace.changes", "neige_workspace_diff"),
    ("neige.workspace.edits", "neige_workspace_log"),
];

/// Tool fields this migration does not rewrite: a provider tool, a plugin tool, a name that only
/// starts like an old one, and an already renamed name.
const KEPT: &[&str] = &[
    "Read",
    "plugin_dev-neige-market_market.quote",
    "neige_plan_list_all",
    "neige_task_ls",
];

async fn run_the_whole_chain(f: &Fx) {
    let mut chain = SEPARATOR_CHAIN.to_vec();
    chain.push(MIGRATION);
    run_the_chain_through(f, 142, &chain).await;
}

#[tokio::test]
async fn stored_view_tool_names_and_recipes_read_back_with_their_verbs() {
    let f = fx().await;
    let t = f.track("verbs").await;
    let planner = f
        .card(&t, "verbs-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "verbs-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("verbs-thread"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;

    let mut seeded = Vec::new();
    for (i, tool) in RENAMED
        .iter()
        .map(|(old, _)| *old)
        .chain(KEPT.iter().copied())
        .enumerate()
    {
        // An old name elsewhere in params proves only `$.item.tool` is rewritten.
        let params = json!({"item": {"id": format!("call-{i}"), "type": "mcpToolCall",
            "server": "neige", "tool": tool, "status": "completed",
            "arguments": {"call": {"tool": "neige_plan_list"}, "text": "neige track state"}}});
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
    let old_body = "Check calm.plan.list, then calm.area.outline.\n\
                    Links: calm.report.links.backlinks; kinds: calm.report.blocks.kinds...\n\
                    Shell: `neige track state` and neige tool list --all.\n\
                    Kept: my_neige_plan_list neige_plan_listing neige_track_state_x \
                    plugin_market_neige_source_list WebFetch\nEnd: calm.source.list";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "verbs".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "plain".into(),
            body: "Uses neige_report_read and WebFetch; the track state is fine.".into(),
        })
        .await
        .unwrap();

    run_the_whole_chain(&f).await;

    let mut migrated_rows = Vec::new();
    for (id, old, mut params) in seeded {
        let expected = RENAMED
            .iter()
            .find(|(from, _)| *from == old)
            .map_or(old, |(_, to)| to);
        params["item"]["tool"] = json!(expected);
        assert_eq!(tool_of(&f, id).await, params, "{old}");
        migrated_rows.push((id, params));
    }

    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        migrated.body,
        "Check neige_task_ls, then neige_area_ls.\n\
         Links: neige_link_ls; kinds: neige_report_describe...\n\
         Shell: `neige track status` and neige tool ls --all.\n\
         Kept: my_neige_plan_list neige_plan_listing neige_track_state_x \
         plugin_market_neige_source_list WebFetch\nEnd: neige_source_ls"
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
        "a recipe without an old view name is not touched"
    );

    // Reapplying the migration changes nothing more.
    rerun_migration(&f, MIGRATION).await;
    for (id, params) in migrated_rows {
        assert_eq!(tool_of(&f, id).await, params);
    }
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

/// The B1 migration alone renames the current names, including `neige_workspace_report` at a
/// sentence end beside `neige_workspace_reports`, and bumps the revision once.
#[tokio::test]
async fn the_verb_migration_bumps_a_recipe_revision_once() {
    let f = fx().await;
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "current".into(),
            body: "List with neige_workspace_reports, read with neige_workspace_report. \
                   Diff: neige_workspace_changes; log: neige_workspace_edits; \
                   cancel: neige_plan_cancel; state: neige_track_state."
                .into(),
        })
        .await
        .unwrap();
    rerun_migration(&f, MIGRATION).await;
    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        migrated.body,
        "List with neige_workspace_ls, read with neige_workspace_cat. \
         Diff: neige_workspace_diff; log: neige_workspace_log; \
         cancel: neige_task_cancel; state: neige_track_status."
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
    assert!(migrated.updated_at > recipe.updated_at);
}
