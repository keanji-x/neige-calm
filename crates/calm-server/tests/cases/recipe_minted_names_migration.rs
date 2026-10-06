//! #2087 C1: the data migration that rewrites the raw upstream tool names of the grandfathered
//! repository plugins in stored recipe bodies to the minted names the model sees. Recipes are
//! seeded with the prose and live sources production stores, the real chain from 0134 through the
//! C1 migration runs, and the bodies are read back through the recipe repository.

use calm_types::model::NewTrackRecipe;

use super::tool_name_separator_migration::{
    SEPARATOR_CHAIN, rerun_migration, run_the_chain_through,
};
use super::track_activity_fixture::{Fx, fx};

const MIGRATION: &str = "recipe minted names";

async fn run_the_whole_chain(f: &Fx) {
    let mut chain = SEPARATOR_CHAIN.to_vec();
    chain.extend([
        "tool verbs",
        "terminal verbs",
        "crud verbs",
        "plugin names",
        "native plugin tool names",
        MIGRATION,
    ]);
    run_the_chain_through(f, 154, &chain).await;
}

#[tokio::test]
async fn stored_recipes_name_legacy_plugin_tools_by_their_minted_names() {
    let f = fx().await;
    // Prose with every boundary the scan distinguishes (full-width punctuation as the stored Chinese
    // recipes use it); the live sources are URI segments.
    let old_body = "Pre-market: call spy.refresh, then read spy.status before deciding. \
                    Save with spy.plan (spy.planned, spy.plan_b and xspy.status are not tools); \
                    the Worker calls spy.execute.\n\
                    Start barra.start\u{ff0c}read barra.status\u{ff1b}stop barra.stop\u{3002} \
                    Charts: barra.series and barra.refresh.\n\
                    Hold with market.holdings.set, list with market.holdings.list, \
                    quote with market.quote, chart with market.series.\n\
                    Views: neige://plugin/dev-neige-paper-trading/spy.nav, \
                    neige://plugin/dev-neige-paper-trading/spy.nav_history, \
                    neige://plugin/dev-neige-paper-trading/spy.status and \
                    neige://plugin/dev-neige-barra/barra.series.\n\
                    Judge outcomes from spy.status.";
    let recipe = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "SPY and cash daily".into(),
            body: old_body.into(),
        })
        .await
        .unwrap();
    // Only live sources: scanned, nothing to rewrite, so the revision stays.
    let untouched = f
        .repo_dyn
        .track_recipe_create(NewTrackRecipe {
            title: "views".into(),
            body: "{\"source\":\"neige://plugin/dev-neige-barra/barra.overview\"} \
                   {\"source\":\"neige://plugin/dev-neige-paper-trading/spy.weights\"} \
                   {\"source\":\"neige://plugin/dev-neige-market/market.series\"}"
                .into(),
        })
        .await
        .unwrap();

    run_the_whole_chain(&f).await;

    let migrated = f
        .repo_dyn
        .track_recipe_get(&recipe.id)
        .await
        .unwrap()
        .expect("recipe");
    assert_eq!(
        migrated.body,
        "Pre-market: call plugin_dev_neige_paper_trading_spy_refresh, then read \
         plugin_dev_neige_paper_trading_spy_status before deciding. \
         Save with plugin_dev_neige_paper_trading_spy_plan (spy.planned, spy.plan_b and \
         xspy.status are not tools); the Worker calls plugin_dev_neige_paper_trading_spy_execute.\n\
         Start plugin_dev_neige_barra_barra_start\u{ff0c}read \
         plugin_dev_neige_barra_barra_status\u{ff1b}stop plugin_dev_neige_barra_barra_stop\u{3002} \
         Charts: plugin_dev_neige_barra_barra_series and plugin_dev_neige_barra_barra_refresh.\n\
         Hold with plugin_dev_neige_market_market_holdings_set, list with \
         plugin_dev_neige_market_market_holdings_list, quote with \
         plugin_dev_neige_market_market_quote, chart with plugin_dev_neige_market_market_series.\n\
         Views: neige://plugin/dev-neige-paper-trading/spy.nav, \
         neige://plugin/dev-neige-paper-trading/spy.nav_history, \
         neige://plugin/dev-neige-paper-trading/spy.status and \
         neige://plugin/dev-neige-barra/barra.series.\n\
         Judge outcomes from plugin_dev_neige_paper_trading_spy_status."
    );
    assert_eq!(migrated.revision, recipe.revision + 1, "{migrated:?}");
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
        "a recipe that names a raw tool only as a live source is not touched"
    );

    // Reapplying the migration changes nothing more.
    rerun_migration(&f, MIGRATION).await;
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
