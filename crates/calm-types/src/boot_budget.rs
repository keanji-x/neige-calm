//! Closed-form wall-clock bounds on the calm-server child's boot autospawn
//! phase.
//!
//! These constants and `const fn`s live at the bottom of the crate DAG for one
//! reason: **two processes need the same arithmetic.** `calm-server` enforces
//! them (its `plugin_host` re-exports every item here, so its call sites,
//! doc links and tests read exactly as they did before #1282), and the
//! `neige-app` host budgets its `/upgrade/apply` healthcheck deadline with
//! [`boot_autospawn_ceiling`] — the autospawn phase runs before the child binds
//! its HTTP listener, so a deadline that does not cover it rolls back boots
//! that would have succeeded.
//!
//! Nothing here does IO or touches a manifest; it is `std::time::Duration`
//! arithmetic only, which is why it fits under this crate's no-sqlx/axum/tokio
//! rule.
//!
//! **What the host links is its own version's numbers.** A `neige-app` binary
//! carries the constants it was compiled with; the release it is about to
//! health-check may have been built with different ones. That residue is real
//! and is not closed here — see #1282.

use std::time::Duration;

/// Hard ceiling on the bring-up (`initialize` + `tools/list`) timeout, enforced
/// here at manifest-parse time.
///
/// **Why a second knob exists at all.** `request_timeout_ms` used to govern two
/// things with opposite constraints, and every round of review patched the
/// arithmetic while the defect reappeared one level up:
///
/// * **bring-up** sits on the inline-awaited boot path (`AppState::new` →
///   `autospawn_enabled` → `spawn_admitted` → `tools/list`), so it must be
///   SHORT and hard-bounded — while it runs, the server does not serve;
/// * **steady-state `tools/call`** is not on the boot path at all and is
///   legitimately long.
///
/// One knob could satisfy neither: clamping it broke long tool calls, and
/// widening the boot budget to respect it made boot latency operator-controlled
/// and unbounded (`"request_timeout_ms": 600000` against a black-holed upstream
/// stalled boot for 20.5 minutes). Splitting them makes the boot bound hold *by
/// construction* for every manifest: `connector_bringup_budget` can never
/// exceed `2 × 15 s + slack`, whatever the manifest asks for.
///
/// (Same shape as `trusted_forge_plugin`, one bit that gates both "may hold a
/// track scope" and "gets the forge credential passthrough". The general lesson:
/// when a constant needs adjusting for the third time, stop adjusting it and
/// look for the second constraint riding on it.)
///
/// 15 s is chosen as "generous for a TLS handshake plus a cold upstream's first
/// response, still short enough that a full slate of dead connectors cannot
/// push boot past [`CONNECTOR_AUTOSPAWN_BUDGET`]".
pub const MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS: u64 = 15_000;

/// Round trips `connect_mcp_http` makes: `initialize` then `tools/list`. The
/// outer bring-up timeout is this multiple of the per-request budget, because
/// `mcp_http.bringup_timeout_ms` configures ONE request, not the whole spawn.
pub const MCP_HTTP_ROUND_TRIPS: u32 = 2;

/// Headroom on top of `MCP_HTTP_ROUND_TRIPS × bringup_timeout_ms` for the work
/// that sits outside ureq's own clock — chiefly `spawn_blocking` queue delay on
/// a busy boot, plus tokio scheduling jitter around the two `.await`s.
///
/// Deliberately a FIXED amount rather than a multiplier: the thing it pays for
/// does not scale with how long the operator is willing to wait for one
/// request. Keeping it small also keeps the bound meaningful for the short
/// timeouts tests configure.
///
/// **Scope: exactly ONE connector's bring-up.** It appears in
/// `connector_bringup_budget`, in the ceiling that constant implies
/// ([`MAX_CONNECTOR_BRINGUP_BUDGET`]), and in the timeout message that explains
/// the arithmetic to an operator. Raising it makes each individual connector
/// wait longer before it is declared `Unavailable`; it does NOT move the
/// per-connector-vs-loop ordering, which is
/// [`CONNECTOR_LOOP_WIDENING_MARGIN`]'s job.
///
/// Until #1194 residual 3 these were one constant serving both purposes — the
/// "one knob carrying two constraints" shape that produced three rounds of
/// "adjust the arithmetic, watch the defect reappear one level up" in this very
/// module. They happen to be equal today; that is a coincidence of the two
/// sizing arguments, not a relationship, and nothing may assume it.
pub const CONNECTOR_BRINGUP_SLACK: Duration = Duration::from_millis(500);

/// The margin [`widened_connector_budget`] adds on top of the widest
/// per-connector bring-up cap when it raises the loop budget.
///
/// It buys ONE property, and it is an ordering property, not a latency one:
/// **the per-connector bound must be the one that fires first.** If the loop
/// budget were widened to exactly `widest`, a single connector running out its
/// own cap would race the loop budget, and whichever lost would decide the
/// operator-facing reason — "connector bring-up timed out after N ms" (true and
/// actionable) versus "budget exhausted before this connector's turn" (which
/// blames earlier connectors that need not exist). That is the boot-vs-enable
/// disagreement described on [`CONNECTOR_AUTOSPAWN_BUDGET`], one level in.
///
/// A FIXED amount, not a multiplier, for a different reason than
/// [`CONNECTOR_BRINGUP_SLACK`]'s: what it must exceed is the loop's own
/// per-iteration overhead between arming the two timers, which is scheduling
/// work of a size unrelated to any configured timeout. A multiplier would also
/// scale straight into [`MAX_CONNECTOR_AUTOSPAWN_WALL`] — boot latency — for no
/// gain.
///
/// **What changing it moves.** The first version of this paragraph said
/// "[`MAX_CONNECTOR_AUTOSPAWN_WALL`] and nothing else", which is wrong in the
/// direction that matters: this constant is not documentation-only, it is
/// evaluated at RUNTIME. The full list, `grep`ed rather than recalled:
///
/// * [`widened_connector_budget`]'s result — which
///   `PluginHost::autospawn_enabled_within` adopts as the loop budget it
///   actually arms, and then feeds to [`connector_phase_ceiling`] for the fence
///   it actually enforces. This is a behaviour change on every boot, not an
///   arithmetic identity;
/// * [`MAX_CONNECTOR_AUTOSPAWN_WALL`] (31.5 s today), which is that expression
///   evaluated at the manifest-validated maximum;
/// * [`boot_autospawn_ceiling`], which sums `MAX_CONNECTOR_AUTOSPAWN_WALL` into
///   the whole-boot bound (71.5 s / 101.5 s / … today).
///
/// Three test sites move with it, and all three are literal-valued, so raising
/// this constant fails them rather than silently absorbing the change:
/// `the_connector_phase_ceiling_is_the_documented_one` and
/// `a_slow_event_store_cannot_hold_boot_past_the_phase_ceiling` in
/// `tests/cases/connector_host.rs`, and `the_app_autospawn_wall_is_the_documented_one`
/// in `tests/cases/plugin_lifecycle_lock.rs`.
pub const CONNECTOR_LOOP_WIDENING_MARGIN: Duration = Duration::from_millis(500);

/// Total wall-clock the *connector* portion of `PluginHost::autospawn_enabled`
/// may consume, across ALL connectors.
///
/// The per-connector bound in `spawn_mcp_http` caps ONE bring-up; autospawn
/// iterates serially and `AppState::new` awaits it inline, so without a bound
/// spanning the loop, N unreachable connectors still stall boot by N × that cap.
/// This is the only construct that makes boot latency independent of how many
/// dead connectors are installed. Connectors that do not get their turn inside
/// the budget land `Unavailable` with a reason that says so — they are not
/// silently skipped, and they are not detached from boot either: acceptance §4
/// #7 requires materialization to have happened before the boot audit loop
/// reads `exposes_tools`, so bring-up must remain inline.
///
/// **Floor is enforced by construction, not by choosing a big number.** A
/// constant floor alone guaranteed that a connector whose own cap exceeded it
/// was cut off by the LOOP budget at boot — blaming "earlier connectors" that
/// need not exist — while `POST /enable`, which has no loop budget, brought the
/// same connector up against its own cap. Boot and enable disagreed about the
/// same manifest. `PluginHost::autospawn_enabled_within` therefore widens this
/// value to `connector_bringup_budget`'s largest value over the enabled
/// connectors (plus [`CONNECTOR_LOOP_WIDENING_MARGIN`], so the per-connector
/// bound is always the one that fires first).
///
/// **And that widening is itself bounded, which is what makes boot latency an
/// invariant rather than a hope.** It was not always: while one manifest field
/// governed both bring-up and `tools/call`, `"request_timeout_ms": 600000`
/// widened this budget to 20.5 minutes — the server did not serve for that
/// long, at the operator's unwitting discretion. The bring-up budget now has
/// its own field with a ceiling validated at manifest parse time
/// ([`MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`]), so
/// [`MAX_CONNECTOR_BRINGUP_BUDGET`] caps `widest` for EVERY manifest that can
/// load, and the widened loop budget can never exceed
/// `max(30 s, MAX_CONNECTOR_BRINGUP_BUDGET + CONNECTOR_LOOP_WIDENING_MARGIN)`.
/// (Naming the constant is not pedantry: since #1194 residual 3 there are TWO
/// 500 ms margins in this module and "slack" no longer identifies either. The
/// widening term is the LOOP one; [`CONNECTOR_BRINGUP_SLACK`] is already inside
/// `MAX_CONNECTOR_BRINGUP_BUDGET`.)
pub const CONNECTOR_AUTOSPAWN_BUDGET: Duration = Duration::from_secs(30);

/// The loop budget `PluginHost::autospawn_enabled_within` actually adopts,
/// given the one it was handed and the widest per-connector bring-up cap among
/// the connectors it is about to iterate.
///
/// A supplied budget is a *floor*, never a ceiling: see
/// [`CONNECTOR_AUTOSPAWN_BUDGET`] for why the widening exists. This is a
/// function rather than an inline `max` in the loop because the widening is half
/// of the boot bound and a test that wants to state the bound must be able to
/// compute it from the same expression production evaluates — restating
/// `max(supplied, widest + CONNECTOR_LOOP_WIDENING_MARGIN)` in a test is a
/// second arithmetic, and a
/// second arithmetic is exactly how `a_slow_event_store_cannot_hold_boot_past_
/// the_phase_ceiling` came to assert a 1.5 s ceiling against a loop that was
/// really running to 1.9 s.
///
/// `const` so that [`MAX_CONNECTOR_AUTOSPAWN_WALL`] can be *this* function
/// applied to the widest loadable inputs rather than a second copy of the same
/// `max` inlined in a const block — which is what it used to be, leaving the
/// helper unpinned by that constant's test.
pub const fn widened_connector_budget(supplied: Duration, widest_bringup: Duration) -> Duration {
    let widened = Duration::from_millis(
        widest_bringup.as_millis() as u64 + CONNECTOR_LOOP_WIDENING_MARGIN.as_millis() as u64,
    );
    // `Ord::max` is not const; compare the raw nanos and return the *original*
    // `supplied` so no precision is lost on the floor side.
    if widened.as_nanos() > supplied.as_nanos() {
        widened
    } else {
        supplied
    }
}

/// Wall-clock allowed for everything the connector phase does **besides**
/// bringing connectors up: the terminal `Unavailable` emission for connectors
/// that never got their turn, and the reconcile emission for one that came up
/// just as the budget ran out. All of those are persisted+broadcast events, so
/// their cost is a slow event store's cost, not this process's.
///
/// It is a budget for the whole tail rather than a per-connector allowance on
/// purpose: a per-connector one is `N ×` again, which is the shape this whole
/// bound exists to remove.
pub const CONNECTOR_RECONCILE_BUDGET: Duration = Duration::from_millis(500);

/// #1238 — wall-clock fence on boot autospawn's initial plugin enumeration.
///
/// Its 40 s composition covers calm-truth's pool-acquisition budget (including
/// the three fresh-connection `after_connect` pragmas,
/// `calm_truth::db::sqlite::SQLITE_ACQUIRE_TIMEOUT_MS`), the SELECT's SQLite
/// busy-handler budget (`calm_truth::db::sqlite::SQLITE_BUSY_TIMEOUT_MS`), and
/// scheduling margin. The fence must be strictly greater than the sum of both
/// bounded waits: if it fires while the database is still inside its own
/// healthy bounded wait, every plugin is silently skipped and no process-local
/// retry exists. Prefer waiting longer over declaring that blackout early.
///
/// The cost is up to 40 s more before the HTTP listener binds in the worst
/// case. That wait remains bounded, which is the guarantee this issue trades
/// for; waiting indefinitely would still keep the listener from binding.
///
/// `pub` and pinned by `the_app_autospawn_wall_is_the_documented_one`: the
/// behavioral test overrides it through `PluginHost::with_plugin_list_wall`
/// so it can prove the fence without waiting out the production allowance.
pub const PLUGIN_LIST_WALL: Duration = Duration::from_secs(40);

/// #1196 S1 review P1-6 — wall-clock fence on ONE `app` plugin's boot autospawn
/// iteration, the local-child mirror of the connector phase fence.
///
/// The `app` branch of `PluginHost::autospawn_enabled_within` had no bound at
/// all. It reaches `PluginHost::await_lifecycle`, which is unbounded on
/// purpose, so a lifecycle guard nobody ever releases hangs boot silently and
/// forever. The design's defence was "boot's only possible contender is a crash
/// supervisor, whose work is itself bounded" — but §5 R6 is explicit that a
/// timing argument is not a proof, and the argument is not even airtight: the
/// guard is reachable from `pub` `try_lock_lifecycle`, and a supervisor's own
/// `spawn_under` can park on a slow event store for as long as that store likes.
///
/// Sized against what an `app` bring-up actually is — fork/exec of a local child
/// plus an `initialize` handshake plus a handful of persisted events — not
/// against a network round trip; connectors have their own, much larger, budget.
/// It is deliberately NOT part of [`MAX_CONNECTOR_AUTOSPAWN_WALL`]: that constant
/// is the *connector phase* ceiling, and app plugins are outside it by design
/// (see the `connector_elapsed` accounting). What this gives is a per-app-plugin
/// bound where there was none, so boot's total is now finite for every plugin
/// kind rather than for one of them — the composed number is
/// [`boot_autospawn_ceiling`].
///
/// `pub` and pinned by `the_app_autospawn_wall_is_the_documented_one`: the only
/// gate that exercises the fence (`a19`) overrides it to 300 ms through
/// `PluginHost::with_app_autospawn_wall`, so without that test a change to
/// this literal would be invisible to CI.
pub const APP_AUTOSPAWN_WALL: Duration = Duration::from_secs(30);

/// The closed-form wall-clock ceiling on all of boot autospawn, for a repo with
/// `app_plugins` enabled `app` plugins.
///
/// It composes exactly one [`PLUGIN_LIST_WALL`], one
/// [`APP_AUTOSPAWN_WALL`] per enabled app, and the single connector-phase
/// ceiling. If enumeration times out, no plugin id is known and the loop is
/// skipped; the list fence is then the only component consumed. The in-memory,
/// await-free registry scan that computes connector widening adds no separate
/// wall-clock term.
///
/// The `app` half is `N ×` on purpose and is not a defect being papered over:
/// app bring-up is a local fork/exec, the plugins are serial, and there is no
/// cross-plugin budget for them the way there is for connectors. What matters is
/// that the total is a closed-form expression of the fenced phases instead of
/// "connectors are bounded and apps are argued about", which is what it was
/// before #1196 S1 review P1-6.
///
/// One expression for the same reason [`connector_phase_ceiling`] is one: the
/// documented number and the enforced number must not be two arithmetics.
/// Asserted against its constituent constants by
/// `the_app_autospawn_wall_is_the_documented_one`.
pub const fn boot_autospawn_ceiling(app_plugins: u32) -> Duration {
    Duration::from_millis(
        PLUGIN_LIST_WALL.as_millis() as u64
            + APP_AUTOSPAWN_WALL.as_millis() as u64 * app_plugins as u64
            + MAX_CONNECTOR_AUTOSPAWN_WALL.as_millis() as u64,
    )
}

/// The wall-clock ceiling on the connector phase of boot, given the loop
/// budget it runs with — spawn, reconcile and every emission inside it.
///
/// This remains scoped to the connector loop. The `plugins_list_all` read that
/// precedes it is fenced separately by [`PLUGIN_LIST_WALL`], but that prelude
/// belongs only to the full [`boot_autospawn_ceiling`] composition and is not a
/// connector-phase cost.
///
/// One expression, so the number that is *documented* and the number that is
/// *enforced* cannot drift: [`MAX_CONNECTOR_AUTOSPAWN_WALL`] is this function
/// applied to the widest budget any loadable manifest can produce, and
/// `PluginHost::autospawn_enabled_within` fences its loop with this function
/// applied to the budget it actually got. Rounds 1-4 each stated a ceiling in
/// prose (30 s, then 30.5 s) while the code computed a different one, because
/// the prose was a separate arithmetic.
pub const fn connector_phase_ceiling(loop_budget: Duration) -> Duration {
    Duration::from_millis(
        loop_budget.as_millis() as u64 + CONNECTOR_RECONCILE_BUDGET.as_millis() as u64,
    )
}

/// The largest wall-clock the connector phase of boot can consume, for **any**
/// set of manifests that load. This is deliberately only the loop ceiling: it
/// excludes the separately fenced plugin enumeration and every app iteration;
/// [`boot_autospawn_ceiling`] is the full boot composition.
///
/// `autospawn_enabled` starts from [`CONNECTOR_AUTOSPAWN_BUDGET`] and widens it
/// to the widest per-connector cap plus [`CONNECTOR_LOOP_WIDENING_MARGIN`] (see
/// there; it is NOT [`CONNECTOR_BRINGUP_SLACK`], which is already folded into
/// the per-connector cap this widens over); that widening is
/// capped by [`MAX_CONNECTOR_BRINGUP_BUDGET`], which manifest-parse-time
/// validation makes structural. This is the composition of the two, and it is
/// what a structural connector-loop bound means. The exact value is computed
/// here rather than duplicated in prose, and asserted against the real loop by
/// `the_connector_phase_ceiling_is_the_documented_one`.
pub const MAX_CONNECTOR_AUTOSPAWN_WALL: Duration = connector_phase_ceiling(
    widened_connector_budget(CONNECTOR_AUTOSPAWN_BUDGET, MAX_CONNECTOR_BRINGUP_BUDGET),
);

/// The largest value `connector_bringup_budget` can return for any manifest
/// that passes `Manifest::validate`.
///
/// This is the constant that makes the boot bound structural. Asserted against
/// the real function in the manifest-driven test suite; if the formula or the
/// ceiling moves without this following, that test fails.
pub const MAX_CONNECTOR_BRINGUP_BUDGET: Duration = Duration::from_millis(
    MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS * MCP_HTTP_ROUND_TRIPS as u64
        + CONNECTOR_BRINGUP_SLACK.as_millis() as u64,
);
