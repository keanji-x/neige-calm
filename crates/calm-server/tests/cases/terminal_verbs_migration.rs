//! #2087 B1b: the data migration that renames the terminal's anchored read to `read` and its
//! lookup to `show`. Rows are seeded under the oldest names production can store (migration 133's
//! `calm.` names), the real chain from 0134 through the B1b migration runs, and the result is read
//! back: each transcript row's params (the fe history matches `$.item.tool` by name) and each
//! recipe through the recipe repository.

use calm_server::model::CardRole;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_types::model::NewTrackRecipe;
use serde_json::json;

use super::neige_tool_name_migration::tool_of;
use super::tool_name_separator_migration::{
    SEPARATOR_CHAIN, rerun_migration, run_the_chain_through,
};
use super::track_activity_fixture::{Fx, fx};

const MIGRATION: &str = "terminal verbs";

/// The stored `$.item.tool` after the whole chain, written out independently of the migrations.
const RENAMED: &[(&str, &str)] = &[
    ("calm.terminal.observe", "neige_terminal_read"),
    ("calm.terminal.resolve", "neige_terminal_show"),
];

/// Tool fields this migration does not rewrite: a sibling terminal tool, a name that only starts
/// like an old one, and an already renamed name.
const KEPT: &[&str] = &[
    "neige_terminal_input",
    "neige_terminal_observed",
    "neige_terminal_read",
];

async fn run_the_whole_chain(f: &Fx) {
    let mut chain = SEPARATOR_CHAIN.to_vec();
    chain.extend(["tool verbs", MIGRATION]);
    run_the_chain_through(f, 143, &chain).await;
}

#[tokio::test]
async fn stored_terminal_reads_and_recipes_read_back_as_read_and_show() {
    let f = fx().await;
    let t = f.track("terminal verbs").await;
    let planner = f
        .card(&t, "terminal-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "terminal-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("terminal-thread"),
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
        // Stored arguments are history: the old flag and replay key stay as they were sent.
        let params = json!({"item": {"id": format!("call-{i}"), "type": "mcpToolCall",
            "server": "neige", "tool": tool, "status": "completed",
            "arguments": {"terminal_id": "t1", "request_id": "r1", "observe": true,
                "next": "neige_terminal_observe"}}});
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
    let old_body = "Find the worker with calm.terminal.resolve, then calm.terminal.observe.\n\
                    Kept: my_neige_terminal_observe neige_terminal_observer \
                    neige_terminal_resolve_x prompts/tools/neige_terminal_observe.md";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "terminal".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "plain".into(),
            body: "Uses neige_terminal_input; observe the screen and resolve the issue.".into(),
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
        "Find the worker with neige_terminal_show, then neige_terminal_read.\n\
         Kept: my_neige_terminal_observe neige_terminal_observer \
         neige_terminal_resolve_x prompts/tools/neige_terminal_observe.md"
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
        "a recipe without an old terminal name is not touched"
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

/// The B1b migration alone renames the current names, each at a sentence end, and bumps the
/// revision once.
#[tokio::test]
async fn the_terminal_verb_migration_bumps_a_recipe_revision_once() {
    let f = fx().await;
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "current".into(),
            body: "Show: neige_terminal_resolve. Read: neige_terminal_observe.".into(),
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
        "Show: neige_terminal_show. Read: neige_terminal_read."
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
    assert!(migrated.updated_at > recipe.updated_at);
}
