//! #1284 S2 — **an `app` plugin really receives its effective configuration**,
//! and one that is missing a required key really does not start.
//!
//! Every test here boots a real [`PluginHost`] over a real child process
//! (`stub-plugin-config`) and asks *the plugin* what it got, through a real
//! MCP `tools/call` on the same connection the handshake ran over. That is the
//! point of the fixture: a test that inspected the kernel's own merge would be
//! re-asserting `plugin_host::config`'s unit tests one layer up and would stay
//! green if the `_meta` injection were deleted outright. Here, deleting it
//! turns `config_meta` into `null` and every positive test fails.
//!
//! Coverage, in pairs plus the two inputs that are nobody's pair:
//!
//!   * defaults with nothing stored / an operator override of the same key —
//!     the two halves of `defaults ⊕ user_config` as observed by the plugin;
//!   * a `required` key that nothing supplies (plugin must not start,
//!     `last_error` must name the key) / the same plugin once the operator
//!     supplies it (starts, and the value arrives);
//!   * no `plugins` row at all — the store *has nothing to say*;
//!   * the store *cannot be read* — a real `DROP TABLE plugins`. The last two
//!     are the same question asked of two different silences, and the kernel
//!     has to answer them differently.
//!
//! ===========================================================================
//! Mutation witness table (every row was applied to this tree, run, and
//! restored; the "red test" column is the **observed** set, not the intended
//! one). Test names are given relative to this module,
//! `plugin_config_delivery::`, unless another module is named.
//!
//! **Selection set.** Every count below is from one and the same set —
//! `test(plugin_config_delivery) or test(config::tests) or test(plugin_routes)
//! or test(plugin_lifecycle_lock) or test(plugin_host_smoke)`, **101 tests,
//! all green unmutated**. The previous revision of this table ran rows 1 and 2
//! over narrower sets (14 and 61 tests), which made their orthogonality claims
//! claims about a smaller universe than row 3's; re-running all rows over the
//! widest set is what makes "and everything else stayed green" comparable
//! across rows. The mutation script and the raw per-row logs are in the S2
//! review record.
//!
//! | # | mutation | red test | red assertion |
//! |---|---|---|---|
//! | 1 | `McpClient::initialize` stops injecting `_meta["dev.neige/config"]` (`if let Some(values) = config` → `config.filter(\|_\| false)`) | `an_unconfigured_plugin_receives_the_manifest_defaults`, `an_operator_value_overrides_the_default_at_the_plugin`, `a_supplied_required_key_lets_the_plugin_start_and_arrives_with_it` — **3 of 101 red** | `left: Null / right: Object {"values": Object {"retries": Number(3), "theme": String("dark")}}` on "the plugin must have been handed a config namespace carrying the manifest's defaults". All three are the one delivery seam asserted at three inputs; every enforcement test (`a_plugin_missing_a_required_key_does_not_come_up`, `a_plugin_with_no_stored_row_…`, `an_unreadable_config_store_…`) and all of S1's `config::tests` stay **green** — the orthogonality claim that matters here, now witnessed over the full 101: enforcement does not ride on delivery |
//! | 2 | `plugin_host::config::missing_required` returns an empty vec unconditionally (`if true { return Vec::new(); }` at the top) | `a_plugin_missing_a_required_key_does_not_come_up`, `a_plugin_with_no_stored_row_is_judged_against_its_manifest_defaults`, **and** `plugin_host::config::tests::missing_required_names_only_the_keys_nothing_supplies` — **3 of 101 red** | `a plugin missing a required key must not start: ()` (the spawn returned `Ok`); `the failure must be observable, not a plugin that looks unenabled` (the row-less test's refusal never happens, so there is no live entry to read); and the S1 unit witness `left: [] / right: ["token", "secondary"]`. Reds in two layers, by construction: S1 owns the function, S2 owns its only production consumer, so one mutation is visible from both — this row is what turns row 24 of `plugin_routes.rs`'s table ("unit-only until S2 lands a consumer") into an end-to-end witness. The three delivery tests stay **green** — row 1's mirror image |
//! | 3 | the missing-required refusal publishes `Crashed` (`drop(guard)` + `emit_crashed_under`) instead of `Unavailable` | `a_plugin_missing_a_required_key_does_not_come_up`, `a_plugin_with_no_stored_row_is_judged_against_its_manifest_defaults` — **2 of 101 red** | `the failure must be observable, not a plugin that looks unenabled` — and note **where** it goes red: not on the `"unavailable"` comparison but one line earlier, on `status()` returning `None` at all. That is the whole argument for the choice, observed rather than asserted: `emit_crashed_under` publishes an event and no live entry, so the refused plugin reads back as if it had never been enabled and there is no `last_error` for any route to show. The refusal itself still happens under this mutation, which is why "it did not start" alone was never a sufficient assertion. (Two reds, not one, since the row-less test reaches the same refusal.) |
//! | 4 | `effective_config_for_spawn` reads `{}` on a repo error instead of failing the spawn (`Err(e) => return Err(e.to_string())` → `Err(_) => Object(default)`) | `an_unreadable_config_store_refuses_the_spawn_and_says_so` — **1 of 101 red** | `the refusal must name the cause: plugin in bad state: plugin_token_set(test.cfg.unreadable): database error: … (code: 1) no such table: main.plugins`. Worth reading closely, because it is not what "reads `{}`" naively predicts: the spawn does not silently succeed — it walks on into `ensure_plugin_token`, which hits the same broken store and dies as a **500 `BadState` naming the token table**. So swallowing the read error does not produce a working plugin, it produces a misattributed kernel fault, and (per row 6) no `unavailable` entry at all. The previous revision of this table recorded this row as `none — known gap`; it is now witnessed |
//! | 5 | `effective_config_for_spawn` treats a missing row as an error (`Ok(None) => Object(default)` → `Ok(None) => return Err(…)`) | `a_plugin_with_no_stored_row_is_judged_against_its_manifest_defaults` — **1 of 101 red** | `a row-less plugin must be judged as configuring nothing, not as unreadable: could not read stored configuration: mutation: no stored row`. This row is the one the previous revision **under-reported**: `boot` seeded a `plugins` row for every case, so the `Ok(None)` arm was unreached and this mutation left the whole table green. `boot_without_stored_row` is what closes it |
//! | 6 | the repo-error exit skips the live entry (`publish_unavailable_under(…)` → plain `drop(guard)`, error unchanged) | `an_unreadable_config_store_refuses_the_spawn_and_says_so` — **1 of 101 red** | `the failure must be observable, not a plugin that looks unenabled` — `status()` is `None`. This is the S2 review's P2-1 in one line: the mutation *is* the code as it stood, a bare `?` on this branch, and it goes red on exactly the assertion row 3 uses to reject `Crashed`. The 503 and the message text survive the mutation untouched, which is why the wire error alone was never a sufficient assertion either |
//!
//! **Where rows 2–6 now live (S3a review P1).** The `required` verdict, the
//! `Unavailable` publication and the repo-error exit were `spawn_admitted`'s
//! own lines when this table was written; the S3a rework moved them into
//! `PluginHost::config_for_spawn_or_unavailable`, which the `cli-query` path
//! now calls as well. The mutations are the same edits at a different address,
//! and each one is now visible from BOTH suites — re-run over the union set,
//! with the cross-kind reds recorded, in `connector_host.rs`'s table (rows 5,
//! 8, 9, 10). Nothing in this file's counts changed; they are simply no longer
//! the whole red set for those rows.
//!
//! Not mutation-testable, stated instead: removing `#[derive(Default)]` from
//! `InitializeMeta` (S2 review P3) is a compile-time guard. Its witness is
//! that `InitializeMeta { expected_echo: Some(t), ..Default::default() }` — a
//! handshake that silently delivers no configuration — stops compiling; there
//! is no runtime behaviour to make red.
//! ===========================================================================

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

const CONFIG_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-config");

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    host: Arc<PluginHost>,
    plugin_id: String,
    /// The concrete repo behind the host, kept so a test can reach past the
    /// `Repo` trait and break the store on purpose — see
    /// `an_unreadable_config_store_refuses_the_spawn`.
    repo: Arc<SqlxRepo>,
    _tmp: tempfile::TempDir,
}

/// Install one `stub-plugin-config` plugin with the given `config_schema` and
/// stored `user_config`, and return a host that has **not** spawned it yet —
/// the spawn is what each test is about, including the one where it fails.
async fn boot(plugin_id: &str, config_schema: Value, user_config: Value) -> Fixture {
    boot_inner(plugin_id, config_schema, Some(user_config)).await
}

/// Same, but with **no `plugins` row at all**: the manifest is in the registry
/// and the DB knows nothing about this id.
///
/// This is the only entry point that exercises `effective_config_for_spawn`'s
/// `Ok(None)` arm, i.e. the one place the "the registry and the DB are separate
/// stores" reasoning in that function's doc is actually load-bearing. Without
/// it the arm is unreached by the whole suite — `boot` seeds a row for every
/// other case — and could be replaced by a panic with the table still green.
async fn boot_without_stored_row(plugin_id: &str, config_schema: Value) -> Fixture {
    boot_inner(plugin_id, config_schema, None).await
}

async fn boot_inner(plugin_id: &str, config_schema: Value, user_config: Option<Value>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(CONFIG_BIN), bin_dir.join("stub")).unwrap();

    // A **file** DB, not `sqlite::memory:`. The store has to be reachable from
    // outside the `Repo` trait for the unreadable-store test to break it with
    // bare SQL, and it has to survive being handed to the host as a trait
    // object. Everything else about these tests is indifferent to the choice.
    let db_path = tmp.path().join("plugins.sqlite3");
    let sqlx_repo = Arc::new(
        SqlxRepo::open(&format!("sqlite://{}?mode=rwc", db_path.display()))
            .await
            .expect("open file-backed sqlite repo"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();

    // `manifest_version: 3` throughout: §2.1's conditional bump is only
    // *required* for schemas that declare `required`, and using it uniformly
    // keeps the fixtures comparable.
    let manifest_json = json!({
        "manifest_version": 3,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Config stub",
        "entrypoint": { "command": "bin/stub" },
        "config_schema": config_schema,
        "theme": { "fg": [216, 219, 226], "bg": [15, 20, 24] },
    });
    // Through the real parser + validator: a `config_schema` that the manifest
    // layer would reject must not be reachable from here either.
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    if let Some(user_config) = user_config {
        repo.plugin_install(calm_server::model::NewPlugin {
            id: plugin_id.into(),
            version: "0.1.0".into(),
            install_path: install_dir.display().to_string(),
            manifest: manifest_json.clone(),
            enabled: true,
            user_config,
        })
        .await
        .expect("seed plugin row");
    }

    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        // Shared with the sibling cases in `tests/plugin_suite.rs`: a cold
        // write context is all any of them needs, and see that helper's doc
        // for why it is not re-spelled here.
        super::plugin_host_smoke::test_write_context(),
    ));

    Fixture {
        host,
        plugin_id: plugin_id.to_string(),
        repo: sqlx_repo,
        _tmp: tmp,
    }
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

/// Ask the **plugin** what the kernel handed it: a live `tools/call` over the
/// same stdio connection the `initialize` handshake ran over. Returns the
/// captured `_meta["dev.neige/config"]` node verbatim (`null` if the kernel
/// sent none).
async fn config_seen_by_plugin(fx: &Fixture) -> Value {
    let client = fx
        .host
        .mcp_client(&fx.plugin_id)
        .await
        .expect("a running app plugin has a live stdio client");
    let result = client
        .tools_call("report_config", json!({}))
        .await
        .expect("tools/call round trip");
    result
        .structured_content
        .and_then(|s| s.get("config_meta").cloned())
        .expect("the stub always reports a `config_meta` key")
}

fn schema_with_default() -> Value {
    json!({
        "type": "object",
        "properties": {
            "theme": { "type": "string", "default": "dark" },
            "retries": { "type": "integer", "default": 3 }
        },
        "additionalProperties": false
    })
}

// ---------------------------------------------------------------------------
// Delivery: defaults, and the operator's override of a default
// ---------------------------------------------------------------------------

/// The `defaults` half of `defaults ⊕ user_config`, witnessed at the plugin.
/// Note what this rules out that a kernel-side assertion cannot: that the
/// merge is computed correctly and then never sent.
#[tokio::test]
async fn an_unconfigured_plugin_receives_the_manifest_defaults() {
    let fx = boot("test.cfg.defaults", schema_with_default(), json!({})).await;
    fx.host.spawn(&fx.plugin_id).await.expect("spawn");
    wait_for_running(&fx.host, &fx.plugin_id).await;

    assert_eq!(
        config_seen_by_plugin(&fx).await,
        json!({ "values": { "theme": "dark", "retries": 3 } }),
        "the plugin must have been handed a config namespace carrying the \
         manifest's defaults"
    );
}

/// The `⊕ user_config` half, at the same key, so the pair differs in exactly
/// one thing: what the operator stored. `retries` is untouched in both, which
/// is what makes "the override replaced the default" a claim about the merge
/// rather than about the payload being rebuilt from scratch.
#[tokio::test]
async fn an_operator_value_overrides_the_default_at_the_plugin() {
    let fx = boot(
        "test.cfg.override",
        schema_with_default(),
        json!({ "theme": "light" }),
    )
    .await;
    fx.host.spawn(&fx.plugin_id).await.expect("spawn");
    wait_for_running(&fx.host, &fx.plugin_id).await;

    assert_eq!(
        config_seen_by_plugin(&fx).await,
        json!({ "values": { "theme": "light", "retries": 3 } }),
        "the operator's value must reach the plugin, and it must not take the \
         untouched default down with it"
    );
}

// ---------------------------------------------------------------------------
// The store itself failing
// ---------------------------------------------------------------------------

/// S2 review P2-1 + the table's row 4 gap: what happens when the config read
/// **errors**, as opposed to returning nothing.
///
/// The break is a real one — `DROP TABLE plugins` through the pool the host
/// holds — and it lands cleanly because this read is the first time the `app`
/// spawn path touches the DB at all (`spawn_admission_check` consults the
/// registry and the in-memory disabled list, nothing else). So every assertion
/// below is about the config read and no earlier step.
///
/// Three claims, the same three the missing-required test makes, because the
/// hole they guard is the same one:
///   1. the spawn is refused rather than silently proceeding on `{}` — which
///      would hand a plugin manifest defaults on top of an operator's real,
///      unread configuration;
///   2. the refusal is observable through `status()` — before the fix this
///      exit was a bare `?`, which dropped the admission guard and left the
///      plugin reading back as if it had never been enabled;
///   3. `last_error` says the store could not be read, so the operator is not
///      told to go fix configuration that may be perfectly fine.
#[tokio::test]
async fn an_unreadable_config_store_refuses_the_spawn_and_says_so() {
    let fx = boot("test.cfg.unreadable", schema_with_default(), json!({})).await;

    sqlx::query("DROP TABLE plugins")
        .execute(fx.repo.pool())
        .await
        .expect("drop the plugins table out from under the host");

    let err = fx
        .host
        .spawn(&fx.plugin_id)
        .await
        .expect_err("a spawn that cannot read stored configuration must not proceed");
    assert!(
        err.to_string()
            .contains("could not read stored configuration"),
        "the refusal must name the cause: {err}"
    );

    let status = fx
        .host
        .status(&fx.plugin_id)
        .await
        .expect("the failure must be observable, not a plugin that looks unenabled");
    assert_eq!(
        status.status.wire_name(),
        "unavailable",
        "nothing was spawned and nothing is watching, same as every other \
         pre-process refusal: got {:?}",
        status.status
    );
    let last_error = status
        .status
        .last_error()
        .expect("`unavailable` must carry the operator's only diagnostic");
    assert!(
        last_error.contains("could not read stored configuration"),
        "`last_error` must say the store failed, not that configuration is \
         missing: {last_error}"
    );
}

// ---------------------------------------------------------------------------
// Enforcement: §2.2's `required`, moved to the consumption side
// ---------------------------------------------------------------------------

fn schema_requiring_token() -> Value {
    json!({
        "type": "object",
        "properties": {
            "token": { "type": "string" },
            "region": { "type": "string", "default": "eu" }
        },
        "required": ["token"],
        "additionalProperties": false
    })
}

/// The negative half. Three separate claims, because "it did not start" alone
/// is satisfied by a kernel that fails for any reason at all:
///   1. the spawn is refused;
///   2. the terminal state is observable through `status()` — i.e. it is not
///      the "looks like it was never enabled" hole;
///   3. `last_error` names the key the operator has to go fill in.
#[tokio::test]
async fn a_plugin_missing_a_required_key_does_not_come_up() {
    let fx = boot("test.cfg.missing", schema_requiring_token(), json!({})).await;

    let err = fx
        .host
        .spawn(&fx.plugin_id)
        .await
        .expect_err("a plugin missing a required key must not start");
    assert!(
        err.to_string().contains("token"),
        "the refusal must name the key: {err}"
    );

    let status = fx
        .host
        .status(&fx.plugin_id)
        .await
        .expect("the failure must be observable, not a plugin that looks unenabled");
    assert_eq!(
        status.status.wire_name(),
        "unavailable",
        "§2.4's terminal state for a plugin that cannot be configured into \
         existence: got {:?}",
        status.status
    );
    let last_error = status
        .status
        .last_error()
        .expect("`unavailable` must carry the operator's only diagnostic");
    assert!(
        last_error.contains("missing required configuration: token"),
        "`last_error` has to say what is missing, verbatim: {last_error}"
    );
    // The defaulted key is not missing — `missing_required` reads the
    // *effective* map — so it must not appear in the refusal.
    assert!(
        !last_error.contains("region"),
        "a key satisfied by its manifest default is not missing: {last_error}"
    );
}

/// The only input that reaches `effective_config_for_spawn`'s `Ok(None)` arm:
/// the manifest is in the registry and the DB has never heard of this plugin.
/// Every other test here seeds a row, which left that arm unreached — replacing
/// it with a `return Err(…)` kept the whole table green.
///
/// What it witnesses is that arm's claim — *a missing row is `{}`, not an
/// error* — at the one place the difference is observable: the `required`
/// verdict. A rowless plugin is judged against its **manifest defaults**, so
/// `region` (defaulted) is not missing and `token` (not defaulted) is, exactly
/// as for a plugin whose row exists and stores `{}`.
///
/// It is asserted here, on the refusal path, rather than as a fourth delivery
/// test, because of a constraint outside this function's reach: an `app` with
/// no `plugins` row cannot complete a spawn at all — `ensure_plugin_token`
/// writes `plugin_tokens`, whose `plugin_id` is `REFERENCES plugins(id)`, so
/// the insert dies on a foreign-key violation (observed:
/// `BadState("plugin_token_set(...): (code: 787) FOREIGN KEY constraint
/// failed")`). The refusal below happens strictly before the token mint, which
/// is what makes the `Ok(None)` arm reachable through it and unreachable
/// through delivery. See `effective_config_for_spawn`'s doc.
#[tokio::test]
async fn a_plugin_with_no_stored_row_is_judged_against_its_manifest_defaults() {
    let fx = boot_without_stored_row("test.cfg.norow", schema_requiring_token()).await;

    let err = fx
        .host
        .spawn(&fx.plugin_id)
        .await
        .expect_err("`token` is supplied by neither a row nor a default");
    let status = fx
        .host
        .status(&fx.plugin_id)
        .await
        .expect("the failure must be observable, not a plugin that looks unenabled");
    let last_error = status
        .status
        .last_error()
        .expect("`unavailable` must carry the operator's only diagnostic");

    assert!(
        last_error.contains("missing required configuration: token"),
        "a row-less plugin must be judged as configuring nothing, not as \
         unreadable: {last_error} (spawn error: {err})"
    );
    assert!(
        !last_error.contains("region"),
        "the manifest default still applies with no row in the DB, so `region` \
         is not missing: {last_error}"
    );
}

/// The positive half of the same pair, on the same manifest: once the operator
/// supplies the key the plugin starts, and the value it was blocked on is
/// exactly what arrives. Without this leg, "missing required blocks the spawn"
/// is satisfiable by a kernel that never starts this plugin at all.
#[tokio::test]
async fn a_supplied_required_key_lets_the_plugin_start_and_arrives_with_it() {
    let fx = boot(
        "test.cfg.supplied",
        schema_requiring_token(),
        json!({ "token": "s3cret-ish" }),
    )
    .await;
    fx.host.spawn(&fx.plugin_id).await.expect("spawn");
    wait_for_running(&fx.host, &fx.plugin_id).await;

    assert_eq!(
        config_seen_by_plugin(&fx).await,
        json!({ "values": { "token": "s3cret-ish", "region": "eu" } }),
        "the key that blocked the spawn must be the key the plugin receives"
    );
}
