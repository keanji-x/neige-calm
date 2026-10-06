//! #2227: the data migration that serves the built-ins' compiled tools as plugin tools. Rows are
//! seeded under the names production stores (the current `neige_` names, and the oldest spellings
//! the chain from 0134 carries to them), the real chain through the #2227 migration runs, and the
//! result is read back: each transcript row's params (the fe history matches `$.item.tool` by
//! name) and each recipe through the recipe repository.

use calm_server::model::CardRole;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_types::model::NewTrackRecipe;
use serde_json::json;

use super::neige_tool_name_migration::tool_of;
use super::tool_name_separator_migration::{
    SEPARATOR_CHAIN, rerun_migration, run_the_chain_through,
};
use super::track_activity_fixture::{Fx, fx};

const MIGRATION: &str = "native plugin tool names";

/// The stored `$.item.tool` after the whole chain, written out independently of the migrations.
const RENAMED: &[(&str, &str)] = &[
    ("neige_dev_publish", "plugin_gitforge_publish"),
    ("neige_calendar_add", "plugin_calendar_add"),
    ("neige_calendar_ls", "plugin_calendar_ls"),
    ("neige_calendar_set", "plugin_calendar_set"),
    ("neige_calendar_rm", "plugin_calendar_rm"),
    // Older spellings reach the new names through the chain.
    ("calm.track.publish", "plugin_gitforge_publish"),
    ("calm.calendar.list", "plugin_calendar_ls"),
];

/// Tool fields this migration does not rewrite: a provider tool, a name that only starts like an
/// old one, an already renamed name, a kernel tool and a manifest plugin tool.
const KEPT: &[&str] = &[
    "Read",
    "neige_calendar_lsx",
    "neige_dev_publisher",
    "plugin_calendar_ls",
    "neige_track_ls",
    "plugin_gitforge_gh_pr_checks",
];

async fn run_the_whole_chain(f: &Fx) {
    let mut chain = SEPARATOR_CHAIN.to_vec();
    chain.extend([
        "tool verbs",
        "terminal verbs",
        "crud verbs",
        "plugin names",
        MIGRATION,
    ]);
    run_the_chain_through(f, 151, &chain).await;
}

#[tokio::test]
async fn stored_native_tool_names_and_recipes_read_back_as_plugin_tools() {
    let f = fx().await;
    let t = f.track("natives").await;
    let planner = f
        .card(&t, "natives-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "natives-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("natives-thread"),
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
            "arguments": {"call": {"tool": "neige_calendar_ls"}, "text": "neige_dev_publish"}}});
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
    let old_body = "Then neige_calendar_ls and add with neige_calendar_add.\n\
                    Edit: neige_calendar_set/neige_calendar_rm. Publish with `neige_dev_publish`.\n\
                    Kept: neige_calendar_lsx my_neige_dev_publish neige_dev_publish_all \
                    plugin_calendar_ls neige_calendar_ls.v2 dev.neige_calendar_add\n\
                    End: neige_calendar_rm.";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "natives".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    let near_miss = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "near miss".into(),
            body: "Only neige_calendar_lsx and my_neige_dev_publish here.".into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "plain".into(),
            body: "Uses plugin_calendar_ls and neige_track_ls; publish the PR.".into(),
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
        "Then plugin_calendar_ls and add with plugin_calendar_add.\n\
         Edit: plugin_calendar_set/plugin_calendar_rm. Publish with `plugin_gitforge_publish`.\n\
         Kept: neige_calendar_lsx my_neige_dev_publish neige_dev_publish_all \
         plugin_calendar_ls neige_calendar_ls.v2 dev.neige_calendar_add\n\
         End: plugin_calendar_rm."
    );
    assert!(migrated.revision > recipe.revision, "{migrated:?}");
    assert!(migrated.updated_at > recipe.updated_at, "{migrated:?}");
    for before in [near_miss, untouched] {
        let after = f
            .repo_dyn
            .track_recipe_get(&before.id)
            .await
            .unwrap()
            .expect("recipe");
        assert_eq!(
            (after.body, after.revision, after.updated_at),
            (before.body, before.revision, before.updated_at),
            "a recipe without an old native name is not touched"
        );
    }

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

/// The #2227 migration alone renames the current names, the SPY recipe's sentence among them, and
/// bumps the revision once.
#[tokio::test]
async fn the_native_name_migration_bumps_a_recipe_revision_once() {
    let f = fx().await;
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "current".into(),
            body: "Then neige_calendar_ls and add only the missing weekly entries. \
                   Publish: neige_dev_publish; edit: neige_calendar_set; drop: neige_calendar_rm; \
                   add: neige_calendar_add."
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
        "Then plugin_calendar_ls and add only the missing weekly entries. \
         Publish: plugin_gitforge_publish; edit: plugin_calendar_set; drop: plugin_calendar_rm; \
         add: plugin_calendar_add."
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
    assert!(migrated.updated_at > recipe.updated_at);
}
