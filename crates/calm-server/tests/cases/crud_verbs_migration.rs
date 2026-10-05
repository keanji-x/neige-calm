//! #2087 B1c: the data migration that gives Calendar and preview the collection verbs and splits
//! the task verdict into accept and reject. Rows are seeded under the oldest names production can
//! store (migration 133's `calm.` names), the real chain from 0134 through the B1c migration runs,
//! and the result is read back: each transcript row's params (the fe history matches `$.item.tool`
//! by name) and each recipe through the recipe repository.

use calm_server::model::CardRole;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_types::model::NewTrackRecipe;
use serde_json::{Value, json};

use super::neige_tool_name_migration::tool_of;
use super::tool_name_separator_migration::{
    SEPARATOR_CHAIN, rerun_migration, run_the_chain_through,
};
use super::track_activity_fixture::{Fx, fx};

const MIGRATION: &str = "crud verbs";

/// Each seeded call, its stored arguments and the `$.item.tool` after the whole chain, written out
/// independently of the migrations.
fn calls() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "calm.task.verdict",
            json!({"attempt_id": "a1", "status": "accepted", "message": "ok"}),
            "neige_task_accept",
        ),
        (
            "calm.task.verdict",
            json!({"attempt_id": "a1", "status": "rejected", "reason": "no", "message": "m"}),
            "neige_task_reject",
        ),
        // A verdict whose status is missing or unknown keeps its name: no verb can be told.
        (
            "calm.task.verdict",
            json!({"attempt_id": "a1", "message": "m"}),
            "neige_task_verdict",
        ),
        (
            "calm.task.verdict",
            json!({"attempt_id": "a1", "status": "maybe", "message": "m"}),
            "neige_task_verdict",
        ),
        (
            "calm.task.verdict",
            json!({"attempt_id": "a1", "status": true, "message": "m"}),
            "neige_task_verdict",
        ),
        (
            "calm.calendar.update",
            json!({"id": "e1", "expected_version": 1, "task": {}, "cancelled": true}),
            "neige_calendar_rm",
        ),
        (
            "calm.calendar.update",
            json!({"id": "e1", "expected_version": 1, "task": {}, "cancelled": false}),
            "neige_calendar_set",
        ),
        // Only a JSON `true` removed the entry; a number never did.
        (
            "calm.calendar.update",
            json!({"id": "e1", "expected_version": 1, "task": {}, "cancelled": 1}),
            "neige_calendar_set",
        ),
        (
            "calm.calendar.list",
            json!({"from": "2026-10-01", "until": "2026-10-08", "timezone": "UTC"}),
            "neige_calendar_ls",
        ),
        (
            "calm.calendar.create",
            json!({"idempotency_key": "k", "task": {}}),
            "neige_calendar_add",
        ),
        (
            "calm.preview.register",
            json!({"key": "fe", "target_port": 5173, "title": "FE"}),
            "neige_preview_add",
        ),
        (
            "calm.preview.unregister",
            json!({"key": "fe"}),
            "neige_preview_rm",
        ),
        // A sibling tool, a name that only starts like an old one, and an already renamed name.
        (
            "neige_task_cancel",
            json!({"key": "k", "status": "accepted"}),
            "neige_task_cancel",
        ),
        ("neige_calendar_lists", json!({}), "neige_calendar_lists"),
        (
            "neige_task_accept",
            json!({"attempt_id": "a1"}),
            "neige_task_accept",
        ),
    ]
}

async fn run_the_whole_chain(f: &Fx) {
    let mut chain = SEPARATOR_CHAIN.to_vec();
    chain.extend(["tool verbs", "terminal verbs", MIGRATION]);
    run_the_chain_through(f, 144, &chain).await;
}

#[tokio::test]
async fn stored_crud_calls_verdicts_and_recipes_read_back_with_their_verbs() {
    let f = fx().await;
    let t = f.track("crud verbs").await;
    let planner = f
        .card(&t, "crud-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "crud-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("crud-thread"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;

    let mut seeded = Vec::new();
    for (i, (tool, arguments, expected)) in calls().into_iter().enumerate() {
        let params = json!({"item": {"id": format!("call-{i}"), "type": "mcpToolCall",
            "server": "neige", "tool": tool, "status": "completed", "arguments": arguments}});
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
        seeded.push((id, tool, params, expected));
    }
    let old_body = "Then calm.calendar.list and create. Accept with calm.task.verdict.\n\
                    Edit with neige_calendar_update. Preview: neige_preview_register, then \
                    neige_preview_unregister; add with neige_calendar_create.\n\
                    Kept: neige_calendar_lists my_neige_task_verdict \
                    prompts/tools/neige_calendar_create.md neige_preview_registered";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "crud".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "plain".into(),
            body: "Uses neige_calendar_ls and neige_task_reject; update the calendar.".into(),
        })
        .await
        .unwrap();

    run_the_whole_chain(&f).await;

    let mut migrated_rows = Vec::new();
    for (id, old, mut params, expected) in seeded {
        // Only the tool field changes; the stored arguments keep their old keys.
        params["item"]["tool"] = json!(expected);
        assert_eq!(tool_of(&f, id).await, params, "{old} -> {expected}");
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
        "Then neige_calendar_ls and create. Accept with neige_task_accept/neige_task_reject.\n\
         Edit with neige_calendar_set/neige_calendar_rm. Preview: neige_preview_add, then \
         neige_preview_rm; add with neige_calendar_add.\n\
         Kept: neige_calendar_lists my_neige_task_verdict \
         prompts/tools/neige_calendar_create.md neige_preview_registered"
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
        "a recipe without an old name is not touched"
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

/// The B1c migration alone renames the current names, each at a sentence end, and bumps the
/// revision once.
#[tokio::test]
async fn the_crud_verb_migration_bumps_a_recipe_revision_once() {
    let f = fx().await;
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "current".into(),
            body: "List: neige_calendar_list. Judge: neige_task_verdict.".into(),
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
        "List: neige_calendar_ls. Judge: neige_task_accept/neige_task_reject."
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
    assert!(migrated.updated_at > recipe.updated_at);
}
