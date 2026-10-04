//! #2003 §6.3: the one data migration that rewrites stored tool names. Every shape observed on the
//! 4140 database is seeded under its old name through the production writers, the embedded
//! migration runs, and the result is read back through the real readers: the activity projector
//! for `neige.user.notify` and the recipe repository for the revision bump.

use calm_server::model::CardRole;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_server::track_activity::NotificationSource;
use calm_types::model::NewTrackRecipe;
use serde_json::{Value, json};

use super::track_activity_fixture::{Fx, fx};

/// The expected rewrite of `$.item.tool`, written out independently of the migration: a row the
/// migration's map drops keeps its old name here and turns this test red.
const RENAMED: &[(&str, &str)] = &[
    ("calm.admin.track_gc", "neige.admin.gc"),
    ("calm.admin.vacuum", "neige.admin.vacuum"),
    ("calm.area.outline", "neige.area.outline"),
    ("calm.calendar.create", "neige.calendar.create"),
    ("calm.calendar.list", "neige.calendar.list"),
    ("calm.calendar.update", "neige.calendar.update"),
    ("calm.plan.cancel", "neige.plan.cancel"),
    ("calm.plan.list", "neige.plan.list"),
    ("calm.preview.register", "neige.preview.register"),
    ("calm.preview.unregister", "neige.preview.unregister"),
    ("calm.ratify.request", "neige.ratify.request"),
    ("calm.report.blocks.kinds", "neige.report.kinds"),
    ("calm.report.commit", "neige.report.commit"),
    ("calm.report.find", "neige.report.find"),
    ("calm.report.links.backlinks", "neige.report.backlinks"),
    ("calm.report.read", "neige.report.read"),
    ("calm.report.tag", "neige.report.tag"),
    ("calm.report.write_markdown", "neige.report.write"),
    ("calm.review.round", "neige.review.round"),
    ("calm.source.capture", "neige.source.capture"),
    ("calm.source.list", "neige.source.list"),
    ("calm.task.complete", "neige.task.complete"),
    ("calm.task.fail", "neige.task.fail"),
    ("calm.task.verdict", "neige.task.verdict"),
    ("calm.terminal.control", "neige.terminal.control"),
    ("calm.terminal.input", "neige.terminal.input"),
    ("calm.terminal.observe", "neige.terminal.observe"),
    ("calm.terminal.open", "neige.terminal.open"),
    ("calm.terminal.resolve", "neige.terminal.resolve"),
    ("calm.track.cat", "neige.track.cat"),
    ("calm.track.cat_at", "neige.track.show"),
    ("calm.track.close", "neige.track.close"),
    ("calm.track.diff", "neige.track.diff"),
    ("calm.track.log", "neige.track.log"),
    ("calm.track.ls", "neige.track.ls"),
    ("calm.track.publish", "neige.track.publish"),
    ("calm.track.rename", "neige.track.rename"),
    ("calm.track.state", "neige.track.state"),
    ("calm.user.notify", "neige.user.notify"),
    ("calm.get_track_state", "neige.track.state"),
    ("calm.update_task_meta", "neige.task.verdict"),
    ("calm.task_completed", "neige.task.complete"),
    ("calm.task_failed", "neige.task.fail"),
    ("calm.dispatch_request", "neige.dispatch.request"),
    ("calm.plan.upsert", "neige.plan.upsert"),
    ("calm.report.blocks.upsert", "neige.report.upsert"),
    ("calm.report.blocks.delete", "neige.report.delete"),
    ("calm.task.replace", "neige.task.replace"),
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_issue_close",
        "plugin.dev.neige.git-forge_gh.issue.close",
    ),
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_issue_view",
        "plugin.dev.neige.git-forge_gh.issue.view",
    ),
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_pr_checks",
        "plugin.dev.neige.git-forge_gh.pr.checks",
    ),
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_pr_diff",
        "plugin.dev.neige.git-forge_gh.pr.diff",
    ),
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_pr_merge",
        "plugin.dev.neige.git-forge_gh.pr.merge",
    ),
];

/// Tool fields the migration leaves alone: a provider tool, a raw plugin name, a qualified name
/// it does not know.
const KEPT: &[&str] = &[
    "Read",
    "plugin.dev-neige-market_market.quote",
    "mcp__calm__plugin_dev_x_unknown",
];

async fn run_the_migration(f: &Fx) {
    let migration = calm_truth::MIGRATOR
        .iter()
        .find(|m| m.description == "neige tool names")
        .expect("the #2003 migration is embedded");
    sqlx::raw_sql(&migration.sql)
        .execute(&f.pool)
        .await
        .expect("apply the #2003 migration");
}

async fn tool_of(f: &Fx, id: i64) -> Value {
    let params: String = sqlx::query_scalar("SELECT params FROM harness_items WHERE id = ?1")
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    serde_json::from_str(&params).unwrap()
}

#[tokio::test]
async fn stored_pr_publication_calls_move_to_dev_without_changing_arguments() {
    let f = fx().await;
    let t = f.track("publish history").await;
    let planner = f
        .card(&t, "publish-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "publish-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("publish-thread"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    let mut seeded = Vec::new();
    for (i, tool) in [
        "calm.track.publish",
        "neige.track.publish",
        "neige.track.rename",
    ]
    .into_iter()
    .enumerate()
    {
        let params = json!({"item": {"tool": tool, "type": "mcpToolCall", "status": "completed",
            "arguments": {"title": "neige.track.publish", "idempotency_key": "keep", "body": "PR"}}});
        let id = f
            .transcript_item(
                &ws,
                &planner,
                &t,
                &format!("publish-{i}"),
                "mcpToolCall",
                "item/completed",
                params.clone(),
            )
            .await;
        seeded.push((id, tool, params));
    }
    run_the_migration(&f).await;
    let migration = calm_truth::MIGRATOR
        .iter()
        .find(|m| m.description == "neige dev publish")
        .unwrap();
    sqlx::raw_sql(&migration.sql)
        .execute(&f.pool)
        .await
        .unwrap();
    for (id, tool, mut params) in seeded {
        if tool != "neige.track.rename" {
            params["item"]["tool"] = json!("neige.dev.publish");
        }
        assert_eq!(tool_of(&f, id).await, params);
    }
    // Reapplying a data migration changes no additional fields.
    sqlx::raw_sql(&migration.sql)
        .execute(&f.pool)
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM harness_items WHERE json_extract(params, '$.item.tool') = 'neige.track.publish'"
    ).fetch_one(&f.pool).await.unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn stored_tool_names_and_recipes_read_back_under_neige() {
    let f = fx().await;
    let t = f.track("history").await;
    let planner = f
        .card(&t, "card-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "ws-planner",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("th-planner"),
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
        // Another `tool` key inside the arguments proves only `$.item.tool` is rewritten.
        let params = json!({"item": {"id": format!("call-{i}"), "type": "mcpToolCall",
            "server": "calm", "tool": tool, "status": "inProgress",
            "arguments": {"call": {"tool": "calm.plan.list"}}}});
        let id = f
            .transcript_item(
                &ws,
                &planner,
                &t,
                &format!("call-{i}"),
                "mcpToolCall",
                "item/started",
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
    let asks = |p: &calm_server::track_activity::ActivityPayload| -> Vec<String> {
        p.items
            .iter()
            .filter(|item| item.source == NotificationSource::Ask)
            .map(|item| item.text.clone())
            .collect()
    };
    assert_eq!(
        asks(&f.recompute(&t).await),
        Vec::<String>::new(),
        "anti-vacuity: the projector does not read the old name"
    );

    let old_body = "Read with calm.calendar.list, capture with calm.source.capture, write with \
                    calm.report.commit or calm.report.write_markdown, diff via calm.track.cat_at.";
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
            body: "Uses neige.report.read only; calm.db stays.".into(),
        })
        .await
        .unwrap();

    run_the_migration(&f).await;

    for (id, old, mut params) in seeded {
        let expected = RENAMED
            .iter()
            .find(|(from, _)| *from == old)
            .map_or(old, |(_, to)| to);
        params["item"]["tool"] = json!(expected);
        assert_eq!(tool_of(&f, id).await, params, "{old}");
    }
    assert_eq!(
        tool_of(&f, notify).await["item"]["tool"],
        json!("neige.user.notify")
    );
    assert_eq!(
        asks(&f.recompute(&t).await),
        ["Ship it?"],
        "the activity projector reads the migrated notify row"
    );

    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        migrated.body,
        "Read with neige.calendar.list, capture with neige.source.capture, write with \
         neige.report.commit or neige.report.write, diff via neige.track.show."
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
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
        "a recipe without a retired name is not touched"
    );
}

/// #2053: active instructions move to report verbs; recorded calls and lookalike
/// identifiers remain historical data. Re-applying the migration is a no-op.
#[tokio::test]
async fn worker_report_recipe_migration_preserves_history_and_revision_anchors() {
    let f = fx().await;
    let t = f.track("report-names").await;
    let planner = f
        .card(&t, "report-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "report-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("report-thread"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    let params = json!({"item":{"id":"report-call", "type":"mcpToolCall",
        "server":"neige", "tool":"neige.task.complete", "status":"completed",
        "arguments":{"attempt_id":"report-attempt", "text":"neige.task.fail"}}});
    let call = f
        .transcript_item(
            &ws,
            &planner,
            &t,
            "report-call",
            "mcpToolCall",
            "item/completed",
            params.clone(),
        )
        .await;
    let old = "Success：`neige.task.complete(attempt_id)`；Failure：neige.task.fail。\n\
               CLI: neige task-completed --attempt-id a; neige task-failed --reason 'why'\n\
               Other: neige.task.completed neige.task.failure plugin.neige.task.complete task-failed-extra\nSentence: neige.task.complete. neige.task.fail...\nFinal: neige.task.fail";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "report".into(),
            body: old.into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "unchanged".into(),
            body: "neige.task.report_success; neige.task.report_failure; neige.task.completed"
                .into(),
        })
        .await
        .unwrap();
    let migration = calm_truth::MIGRATOR
        .iter()
        .find(|m| m.description == "worker report recipe names")
        .expect("#2053 recipe migration is embedded");
    sqlx::raw_sql(&migration.sql)
        .execute(&f.pool)
        .await
        .unwrap();
    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        migrated.body,
        "Success：`neige.task.report_success(attempt_id)`；Failure：neige.task.report_failure。\n\
         CLI: neige task-report-success --attempt-id a; neige task-report-failure --reason 'why'\n\
         Other: neige.task.completed neige.task.failure plugin.neige.task.complete task-failed-extra\nSentence: neige.task.report_success. neige.task.report_failure...\nFinal: neige.task.report_failure"
    );
    assert_eq!(migrated.revision, recipe.revision + 1);
    assert!(migrated.updated_at > recipe.updated_at);
    assert_eq!(
        tool_of(&f, call).await,
        params,
        "recorded calls are not executable instructions"
    );
    sqlx::raw_sql(&migration.sql)
        .execute(&f.pool)
        .await
        .unwrap();
    let repeated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (repeated.body, repeated.revision, repeated.updated_at),
        (migrated.body, migrated.revision, migrated.updated_at)
    );
    let kept = f
        .repo_dyn
        .track_recipe_get(&untouched.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (kept.body, kept.revision, kept.updated_at),
        (untouched.body, untouched.revision, untouched.updated_at)
    );
}
