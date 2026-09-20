//! Closed-form wall-clock bounds on the calm-server child's boot autospawn phase.
//! `Duration` arithmetic only: `calm-server` enforces these and `neige-app` budgets its healthcheck deadline with them.

use std::time::Duration;

/// Hard ceiling on the bring-up (`initialize` + `tools/list`) timeout, enforced here at manifest-parse time.
pub const MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS: u64 = 15_000;

/// Baseline round trips `connect_mcp_http` makes: `initialize` then the first `tools/list` page.
pub const MCP_HTTP_ROUND_TRIPS: u32 = 2;

/// Headroom on top of `MCP_HTTP_ROUND_TRIPS × bringup_timeout_ms` for the work that sits outside
/// ureq's own clock; scoped to exactly ONE connector's bring-up.
pub const CONNECTOR_BRINGUP_SLACK: Duration = Duration::from_millis(500);

/// The margin [`widened_connector_budget`] adds on top of the widest per-connector bring-up cap when
/// it raises the loop budget, so the per-connector bound is always the one that fires first.
pub const CONNECTOR_LOOP_WIDENING_MARGIN: Duration = Duration::from_millis(500);

/// Total wall-clock the *connector* portion of `PluginHost::autospawn_enabled` may consume, across ALL connectors.
pub const CONNECTOR_AUTOSPAWN_BUDGET: Duration = Duration::from_secs(30);

/// The loop budget `PluginHost::autospawn_enabled_within` actually adopts, given the one it was
/// handed and the widest per-connector bring-up cap; a supplied budget is a floor, never a ceiling.
pub const fn widened_connector_budget(supplied: Duration, widest_bringup: Duration) -> Duration {
    let widened = Duration::from_millis(
        widest_bringup.as_millis() as u64 + CONNECTOR_LOOP_WIDENING_MARGIN.as_millis() as u64,
    );
    // `Ord::max` is not const; return the original `supplied` so no precision is lost on the floor side.
    if widened.as_nanos() > supplied.as_nanos() {
        widened
    } else {
        supplied
    }
}

/// Wall-clock allowed for everything the connector phase does **besides** bringing connectors up.
pub const CONNECTOR_RECONCILE_BUDGET: Duration = Duration::from_millis(500);

/// Wall-clock fence on boot autospawn's initial plugin enumeration. Must stay strictly greater than
/// the pool-acquisition plus SQLite busy-handler budgets, or every plugin is silently skipped with no retry.
pub const PLUGIN_LIST_WALL: Duration = Duration::from_secs(40);

/// Wall-clock fence on ONE `app` plugin's boot autospawn iteration, the local-child mirror of the
/// connector phase fence.
pub const APP_AUTOSPAWN_WALL: Duration = Duration::from_secs(30);

/// The closed-form wall-clock ceiling on all of boot autospawn, for a repo with `app_plugins` enabled `app` plugins.
pub const fn boot_autospawn_ceiling(app_plugins: u32) -> Duration {
    Duration::from_millis(
        PLUGIN_LIST_WALL.as_millis() as u64
            + APP_AUTOSPAWN_WALL.as_millis() as u64 * app_plugins as u64
            + MAX_CONNECTOR_AUTOSPAWN_WALL.as_millis() as u64,
    )
}

/// The wall-clock ceiling on the connector phase of boot, given the loop budget it runs with.
pub const fn connector_phase_ceiling(loop_budget: Duration) -> Duration {
    Duration::from_millis(
        loop_budget.as_millis() as u64 + CONNECTOR_RECONCILE_BUDGET.as_millis() as u64,
    )
}

/// The largest wall-clock the connector phase of boot can consume, for **any** set of manifests that load.
pub const MAX_CONNECTOR_AUTOSPAWN_WALL: Duration = connector_phase_ceiling(
    widened_connector_budget(CONNECTOR_AUTOSPAWN_BUDGET, MAX_CONNECTOR_BRINGUP_BUDGET),
);

/// The largest value `connector_bringup_budget` can return for any manifest that passes `Manifest::validate`.
pub const MAX_CONNECTOR_BRINGUP_BUDGET: Duration = Duration::from_millis(
    MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS * MCP_HTTP_ROUND_TRIPS as u64
        + CONNECTOR_BRINGUP_SLACK.as_millis() as u64,
);
