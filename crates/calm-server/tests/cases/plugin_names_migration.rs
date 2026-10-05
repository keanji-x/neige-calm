//! #2087 B5: the data migration that renames the two built-in plugin ids and respells every stored
//! plugin tool name as the kernel mints it. Rows are seeded the way production stores them at
//! migration 133/140 (both built-in rows with kv, a token, a Track scope, forge operation keys,
//! transcript calls and a recipe), the real chain from 0134 through the B5 migration runs, and the
//! result is read back; then the built-ins come up through `reconcile_builtins` with no orphan row.

use std::sync::Arc;

use calm_server::model::CardRole;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::plugin_results::registry_name;
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_types::model::NewTrackRecipe;
use serde_json::json;

use super::neige_tool_name_migration::tool_of;
use super::tool_name_separator_migration::{
    SEPARATOR_CHAIN, rerun_migration, run_the_chain_through,
};
use super::track_activity_fixture::{Fx, fx};

const MIGRATION: &str = "plugin names";

/// Each seeded `$.item.tool` and its value after the whole chain, written out independently of
/// the migrations.
const CALLS: &[(&str, &str)] = &[
    // Migration 133 stored `plugin.` names; 0141 made them `plugin_`.
    (
        "plugin.dev.neige.git-forge_gh.pr.checks",
        "plugin_gitforge_gh_pr_checks",
    ),
    (
        "plugin_dev.neige.git-forge_gh.issue.comments",
        "plugin_gitforge_gh_issue_comments",
    ),
    (
        "plugin_dev.neige.git-forge_git.worktree.add",
        "plugin_gitforge_git_worktree_add",
    ),
    (
        "mcp__calm__plugin_dev_neige_git-forge_gh_pr_checks",
        "plugin_gitforge_gh_pr_checks",
    ),
    // External plugins keep their ids; only the minted spelling changes.
    (
        "plugin.dev-neige-market_market.quote",
        "plugin_dev_neige_market_market_quote",
    ),
    (
        "plugin_mcp-wisburg-mcp-server-49abefc5_get-report-detail",
        "plugin_mcp_wisburg_mcp_server_49abefc5_get_report_detail",
    ),
    ("plugin_cli-longbridge_kline", "plugin_cli_longbridge_kline"),
    // Not ours, or already minted: kept.
    (
        "plugin_management.search_plugins",
        "plugin_management.search_plugins",
    ),
    ("plugin_gitforge_gh_pr_diff", "plugin_gitforge_gh_pr_diff"),
    ("neige_track_ls", "neige_track_ls"),
];

async fn run_the_whole_chain(f: &Fx) {
    let mut chain = SEPARATOR_CHAIN.to_vec();
    chain.extend(["tool verbs", "terminal verbs", "crud verbs", MIGRATION]);
    run_the_chain_through(f, 148, &chain).await;
}

async fn seed_plugin(f: &Fx, id: &str, install_path: &str, enabled: bool) {
    sqlx::query(
        "INSERT INTO plugins (id, version, install_path, manifest, enabled, user_config, \
         installed_at, updated_at) VALUES (?1, '0.1.0', ?2, ?3, ?4, '{\"k\":1}', 5, 6)",
    )
    .bind(id)
    .bind(install_path)
    .bind(json!({ "id": id }).to_string())
    .bind(enabled)
    .execute(&f.pool)
    .await
    .unwrap();
}

async fn scalar_rows(f: &Fx, sql: &str) -> Vec<(String, String)> {
    sqlx::query_as(sql).fetch_all(&f.pool).await.unwrap()
}

#[tokio::test]
async fn stored_plugin_ids_and_names_read_back_as_minted() {
    let f = fx().await;
    let t = f.track("plugin names").await;
    let planner = f
        .card(&t, "names-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "names-session",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("names-thread"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;

    seed_plugin(&f, "dev.neige.calendar", "builtin:dev.neige.calendar", true).await;
    seed_plugin(
        &f,
        "dev.neige.git-forge",
        "builtin:dev.neige.git-forge",
        true,
    )
    .await;
    seed_plugin(&f, "dev-neige-market", "/plugins/dev-neige-market", true).await;
    for (plugin, key) in [
        ("dev.neige.calendar", "entry:e1"),
        ("dev.neige.calendar", "receipt:[\"card:c\",\"spy\"]"),
        ("dev-neige-market", "cursor"),
    ] {
        sqlx::query(
            "INSERT INTO plugin_kv (plugin_id, key, value, updated_at) VALUES (?1, ?2, '{}', 7)",
        )
        .bind(plugin)
        .bind(key)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    for plugin in ["dev.neige.git-forge", "dev-neige-market"] {
        sqlx::query(
            "INSERT INTO plugin_tokens (plugin_id, hashed_token, expires_at) VALUES (?1, 'h', 9)",
        )
        .bind(plugin)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let scoped = f.track("scoped").await;
    let market = f.track("market").await;
    for (track, scope) in [
        (&scoped, "dev.neige.git-forge"),
        (&market, "dev-neige-market"),
    ] {
        sqlx::query("UPDATE tracks SET plugin_scope = ?1 WHERE id = ?2")
            .bind(scope)
            .bind(track)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    for (id, key) in [
        (
            "op-forge",
            Some("dev.neige.git-forge:t:c:gh.pr.checks:owner/repo:42"),
        ),
        (
            "op-delivery",
            Some("dev.neige.git-forge:t:c:git.commit:d:d1"),
        ),
        ("op-other", Some("planner-terminal:x")),
        ("op-unkeyed", None),
    ] {
        sqlx::query(
            "INSERT INTO operations (id, operation_key, kind, idempotency_key, payload_hash, \
             target_type, target_json, payload_json, phase, created_at_ms, updated_at_ms) \
             VALUES (?1, ?1, 'forge-action', ?2, 'h', 'card', '{}', '{}', 'succeeded', 1, 1)",
        )
        .bind(id)
        .bind(key)
        .execute(&f.pool)
        .await
        .unwrap();
    }

    let mut seeded = Vec::new();
    for (i, (tool, expected)) in CALLS.iter().enumerate() {
        let server = if tool.starts_with("plugin_management.") {
            "codex_apps"
        } else {
            "neige"
        };
        let params = json!({"item": {"id": format!("call-{i}"), "type": "mcpToolCall",
            "server": server, "tool": tool, "status": "completed", "arguments": {"repo": "o/r"}}});
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
        seeded.push((id, params, *expected));
    }
    let old_body = "Wait with plugin_dev.neige.git-forge_gh.pr.checks. Read \
                    plugin_dev.neige.git-forge_gh.issue.comments, then post with \
                    plugin_dev.neige.git-forge_gh.issue.comment.\n\
                    See neige://plugin/dev.neige.calendar/entries.\n\
                    Kept: neige://plugin/dev-neige-market/market.series \
                    xplugin_dev.neige.git-forge_git.commit";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "names".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "plain".into(),
            body:
                "Uses plugin_gitforge_gh_pr_checks and neige://plugin/dev-neige-barra/barra.series"
                    .into(),
        })
        .await
        .unwrap();

    run_the_whole_chain(&f).await;

    // `plugins`: the built-ins under their words with every column but the id kept; no old row.
    let plugins: Vec<(String, String, String, bool, String)> = sqlx::query_as(
        "SELECT id, install_path, manifest, enabled, user_config FROM plugins ORDER BY id",
    )
    .fetch_all(&f.pool)
    .await
    .unwrap();
    let ids: Vec<&str> = plugins.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(ids, ["calendar", "dev-neige-market", "gitforge"]);
    for (id, install_path, manifest, enabled, user_config) in &plugins {
        if id != "dev-neige-market" {
            assert_eq!(install_path, &format!("builtin:{id}"));
            assert_eq!(manifest, &json!({ "id": id }).to_string());
        }
        assert!(enabled, "{id}");
        assert_eq!(user_config, "{\"k\":1}", "{id}");
    }
    assert_eq!(
        scalar_rows(
            &f,
            "SELECT plugin_id, key FROM plugin_kv ORDER BY plugin_id, key"
        )
        .await,
        [
            ("calendar".to_string(), "entry:e1".to_string()),
            (
                "calendar".to_string(),
                "receipt:[\"card:c\",\"spy\"]".to_string()
            ),
            ("dev-neige-market".to_string(), "cursor".to_string()),
        ]
    );
    assert_eq!(
        scalar_rows(
            &f,
            "SELECT plugin_id, hashed_token FROM plugin_tokens ORDER BY plugin_id"
        )
        .await,
        [
            ("dev-neige-market".to_string(), "h".to_string()),
            ("gitforge".to_string(), "h".to_string()),
        ]
    );
    let scopes = scalar_rows(
        &f,
        "SELECT id, plugin_scope FROM tracks WHERE plugin_scope IS NOT NULL ORDER BY plugin_scope",
    )
    .await;
    assert_eq!(
        scopes,
        [
            (market.clone(), "dev-neige-market".to_string()),
            (scoped.clone(), "gitforge".to_string()),
        ]
    );
    let keys: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, idempotency_key FROM operations ORDER BY id")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        keys,
        [
            (
                "op-delivery".to_string(),
                Some("gitforge:t:c:git.commit:d:d1".to_string())
            ),
            (
                "op-forge".to_string(),
                Some("gitforge:t:c:gh.pr.checks:owner/repo:42".to_string())
            ),
            (
                "op-other".to_string(),
                Some("planner-terminal:x".to_string())
            ),
            ("op-unkeyed".to_string(), None),
        ],
        "the idem_key part, `gh.pr.checks:…`, is byte for byte the same"
    );

    // Transcript names: only the tool field changes, and an external name is exactly the minting.
    assert_eq!(
        registry_name("mcp-wisburg-mcp-server-49abefc5", "get-report-detail"),
        "plugin_mcp_wisburg_mcp_server_49abefc5_get_report_detail"
    );
    assert_eq!(
        registry_name("dev-neige-market", "market.quote"),
        "plugin_dev_neige_market_market_quote"
    );
    assert_eq!(
        registry_name("gitforge", "gh_pr_checks"),
        "plugin_gitforge_gh_pr_checks"
    );
    let mut migrated_rows = Vec::new();
    for (id, mut params, expected) in seeded {
        let old = params["item"]["tool"].clone();
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
        "Wait with plugin_gitforge_gh_pr_checks. Read plugin_gitforge_gh_issue_comments, then \
         post with plugin_gitforge_gh_issue_comment.\n\
         See neige://plugin/calendar/entries.\n\
         Kept: neige://plugin/dev-neige-market/market.series \
         xplugin_dev.neige.git-forge_git.commit"
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
        "a recipe without an old built-in name is not touched"
    );

    // Reapplying the migration changes nothing more.
    rerun_migration(&f, MIGRATION).await;
    for (id, params) in &migrated_rows {
        assert_eq!(&tool_of(&f, *id).await, params);
    }
    let repeated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        (repeated.body, repeated.revision),
        (migrated.body, migrated.revision)
    );
    let ids_again: Vec<String> = sqlx::query_scalar("SELECT id FROM plugins ORDER BY id")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    assert_eq!(ids_again, ["calendar", "dev-neige-market", "gitforge"]);

    // The built-ins come up through the production reconcile onto the moved rows: no orphan,
    // `enabled` and the calendar kv kept.
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty().with_builtins()),
        f.repo_dyn.clone(),
        tmp.path().join("plugins"),
        tmp.path().join("data"),
        Vec::new(),
        f.events.clone(),
        f.write.clone(),
    ));
    host.reconcile_builtins().await.unwrap();
    let after: Vec<(String, String, bool)> =
        sqlx::query_as("SELECT id, install_path, enabled FROM plugins ORDER BY id")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        after,
        [
            ("calendar".to_string(), "builtin:calendar".to_string(), true),
            (
                "dev-neige-market".to_string(),
                "/plugins/dev-neige-market".to_string(),
                true
            ),
            ("gitforge".to_string(), "builtin:gitforge".to_string(), true),
        ]
    );
    let calendar_kv: i64 =
        sqlx::query_scalar("SELECT count(*) FROM plugin_kv WHERE plugin_id = 'calendar'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(calendar_kv, 2);
}
