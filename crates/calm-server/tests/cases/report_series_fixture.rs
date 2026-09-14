//! #1628 S2 — shared fixture for the `chart.series` resolver tests.
//!
//! Builds on `mcp_track_report::boot()` (one track, planner + report cards,
//! the real tool registry) and adds a plugin host running the
//! file-programmed `plugin-host-stub-series` plugin under the id the design
//! uses (`dev-neige-market`), a second stub `aa` exposing `b_c` (the A11
//! underscore probe), an injectable clock, and a `SeriesResolver` wired into
//! a fresh `AppContext`.
//!
//! Replies are programmed by writing `reply.json` in the plugin's control
//! directory; every `tools/call` the plugin receives is one line of
//! `calls.jsonl`, which is what the "how many times was the plugin called"
//! assertions read.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::NewPlugin;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::report_series::{Enqueue, Job, ResolveOutcome, SeriesRequest, SeriesResolver};
use calm_types::report_blocks::KIND_CHART_SERIES;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};

use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity};

pub(crate) const SERIES_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-series");
pub(crate) const MARKET_PLUGIN_ID: &str = "dev-neige-market";
pub(crate) const SERIES_TOOL: &str = "market.series";
pub(crate) const SOURCE: &str = "neige://plugin/dev-neige-market/market.series";
/// Exposed with `readOnlyHint: false` (A11).
pub(crate) const WRITE_TOOL_SOURCE: &str = "neige://plugin/dev-neige-market/market.holdings.set";
/// Exposed with `kind: forge-action` (A11).
pub(crate) const FORGE_TOOL_SOURCE: &str = "neige://plugin/dev-neige-market/execute";
/// A tool the manifest does not list (`NotExposed`).
pub(crate) const UNEXPOSED_SOURCE: &str = "neige://plugin/dev-neige-market/market.nothing";
/// A plugin id no manifest carries (`NotInstalled`).
pub(crate) const UNINSTALLED_SOURCE: &str = "neige://plugin/nobody/market.series";
/// The underscore probe: plugin `aa` exposes `b_c`; a `plugin.aa_b_c` re-parse
/// would hit it, an exact lookup of `aa_b` must not.
pub(crate) const UNDERSCORE_PLUGIN_ID: &str = "aa";
pub(crate) const UNDERSCORE_TOOL: &str = "b_c";
pub(crate) const UNDERSCORE_SOURCE: &str = "neige://plugin/aa_b/c";
pub(crate) const TOOL_REPORT_READ: &str = "calm.report.read";

/// 2026-09-14T12:00:00Z — a Monday; "yesterday UTC" is Sunday 2026-09-13.
pub(crate) const T0_MS: i64 = 1_789_387_200_000;
pub(crate) const DAY_MS: i64 = 86_400_000;

pub(crate) struct FixtureOptions {
    /// `true` → `SeriesResolver::new_unstarted` (jobs are recorded, run them
    /// with [`SeriesFixture::run_recorded_jobs`]); `false` → lanes drain.
    pub unstarted: bool,
    pub resolve_timeout: Duration,
    /// Spawn the market plugin at boot. `false` leaves it installed but
    /// stopped (`NotRunning`).
    pub spawn_market: bool,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            unstarted: true,
            resolve_timeout: Duration::from_secs(5),
            spawn_market: true,
        }
    }
}

pub(crate) struct SeriesFixture {
    pub boot: Boot,
    pub plugin_host: Arc<PluginHost>,
    /// Control directory of the market plugin.
    pub market_dir: PathBuf,
    /// Control directory of the `aa` plugin.
    pub underscore_dir: PathBuf,
    /// The injected clock, milliseconds.
    pub clock: Arc<AtomicI64>,
    _tmp: TempDir,
}

fn install_stub(
    plugins_dir: &Path,
    control_root: &Path,
    id: &str,
    manifest_json: Value,
) -> (Manifest, PathBuf, PathBuf) {
    let install_dir = plugins_dir.join(id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::os::unix::fs::symlink(Path::new(SERIES_BIN), bin_dir.join("stub"))
        .expect("symlink series stub");
    let control_dir = control_root.join(id);
    std::fs::create_dir_all(&control_dir).expect("create control dir");
    let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
    (manifest, install_dir, control_dir)
}

fn market_manifest(control_dir: &Path) -> Value {
    json!({
        "manifest_version": 2,
        "id": MARKET_PLUGIN_ID,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Fake market",
        "entrypoint": {
            "command": "bin/stub",
            "env": { "STUB_SERIES_DIR": control_dir.display().to_string() }
        },
        "exposes_tools": [
            {
                "name": SERIES_TOOL,
                "description": "series by window",
                "annotations": { "readOnlyHint": true, "openWorldHint": true }
            },
            {
                "name": "market.holdings.set",
                "description": "writes holdings",
                "annotations": { "readOnlyHint": false }
            },
            { "name": "execute", "description": "forge", "kind": "forge-action" }
        ],
        "permissions": {}
    })
}

fn underscore_manifest(control_dir: &Path) -> Value {
    json!({
        "manifest_version": 1,
        "id": UNDERSCORE_PLUGIN_ID,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Underscore probe",
        "entrypoint": {
            "command": "bin/stub",
            "env": { "STUB_SERIES_DIR": control_dir.display().to_string() }
        },
        "exposes_tools": [
            {
                "name": UNDERSCORE_TOOL,
                "description": "would be hit by a plugin.aa_b_c re-parse",
                "annotations": { "readOnlyHint": true }
            }
        ],
        "permissions": {}
    })
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "plugin {id} did not reach Running"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

impl SeriesFixture {
    pub async fn boot(options: FixtureOptions) -> Self {
        let mut boot = boot().await;
        let tmp = tempfile::Builder::new()
            .prefix("rs")
            .tempdir_in(if std::env::temp_dir().as_os_str().len() <= 40 {
                std::env::temp_dir()
            } else {
                PathBuf::from("/tmp")
            })
            .expect("tempdir");
        let plugins_dir = tmp.path().join("plugins");
        let plugins_data_dir = tmp.path().join("plugins-data");
        let control_root = tmp.path().join("control");
        std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");

        let market_control = control_root.join(MARKET_PLUGIN_ID);
        let (market_manifest, market_install, market_dir) = install_stub(
            &plugins_dir,
            &control_root,
            MARKET_PLUGIN_ID,
            market_manifest(&market_control),
        );
        let underscore_control = control_root.join(UNDERSCORE_PLUGIN_ID);
        let (underscore_manifest, underscore_install, underscore_dir) = install_stub(
            &plugins_dir,
            &control_root,
            UNDERSCORE_PLUGIN_ID,
            underscore_manifest(&underscore_control),
        );
        let registry = PluginRegistry::builder()
            .with(market_manifest, Some(market_install.clone()))
            .with(underscore_manifest, Some(underscore_install.clone()))
            .build();
        for (id, install) in [
            (MARKET_PLUGIN_ID, &market_install),
            (UNDERSCORE_PLUGIN_ID, &underscore_install),
        ] {
            boot.repo
                .plugin_install(NewPlugin {
                    id: id.into(),
                    version: "0.1.0".into(),
                    install_path: install.display().to_string(),
                    manifest: json!({}),
                    enabled: true,
                    user_config: json!({}),
                })
                .await
                .expect("seed plugin row");
        }
        let plugin_host = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            boot.repo.clone(),
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            boot.ctx.events.clone(),
            boot.ctx.write.clone(),
        ));
        if options.spawn_market {
            plugin_host
                .spawn(MARKET_PLUGIN_ID)
                .await
                .expect("spawn market stub");
            wait_for_running(&plugin_host, MARKET_PLUGIN_ID).await;
        }
        plugin_host
            .spawn(UNDERSCORE_PLUGIN_ID)
            .await
            .expect("spawn underscore stub");
        wait_for_running(&plugin_host, UNDERSCORE_PLUGIN_ID).await;

        let clock = Arc::new(AtomicI64::new(T0_MS));
        let now_clock = clock.clone();
        let pool = boot.repo.sqlite_pool();
        let resolver = if options.unstarted {
            SeriesResolver::new_unstarted(pool)
        } else {
            SeriesResolver::new(pool)
        }
        .with_resolve_timeout(options.resolve_timeout)
        .with_now(Arc::new(move || now_clock.load(Ordering::SeqCst)));

        let plugin_host_cell = Arc::new(OnceCell::new());
        assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
        let mut ctx: AppContext = (*boot.ctx).clone();
        ctx.plugin_host = plugin_host_cell;
        ctx.series_resolver = Arc::new(resolver);
        boot.ctx = Arc::new(ctx);

        Self {
            boot,
            plugin_host,
            market_dir,
            underscore_dir,
            clock,
            _tmp: tmp,
        }
    }

    pub fn ctx(&self) -> &Arc<AppContext> {
        &self.boot.ctx
    }

    pub fn resolver(&self) -> &Arc<SeriesResolver> {
        &self.boot.ctx.series_resolver
    }

    pub fn track_id(&self) -> &str {
        self.boot.track_id.as_str()
    }

    pub fn set_clock(&self, now_ms: i64) {
        self.clock.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance_clock(&self, delta_ms: i64) {
        self.clock.fetch_add(delta_ms, Ordering::SeqCst);
    }

    // --- plugin programming ------------------------------------------------

    /// Program the market plugin's next replies (see the stub's header for
    /// the shapes).
    pub fn program(&self, program: Value) {
        write_program(&self.market_dir, program);
    }

    pub fn reply_structured(&self, structured: Value) {
        self.program(json!({ "mode": "structured", "structured": structured }));
    }

    pub fn reply_hang(&self) {
        self.program(json!({ "mode": "hang" }));
    }

    pub fn reply_is_error(&self, text: &str) {
        self.program(json!({ "mode": "is_error", "text": text }));
    }

    /// Every `tools/call` the market plugin received, in order: the
    /// request `params` (`name`, `arguments`, `_meta`).
    pub fn calls(&self) -> Vec<Value> {
        read_calls(&self.market_dir)
    }

    pub fn call_count(&self) -> usize {
        self.calls().len()
    }

    pub fn underscore_call_count(&self) -> usize {
        read_calls(&self.underscore_dir).len()
    }

    /// Wait until the market plugin has received at least `n` calls.
    pub async fn wait_for_calls(&self, n: usize, within: Duration) {
        let deadline = Instant::now() + within;
        while self.call_count() < n {
            assert!(
                Instant::now() < deadline,
                "market plugin received {} call(s), waited for {n}",
                self.call_count()
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    // --- report writes and reads ------------------------------------------

    /// The read snapshot WITHOUT hydration — fixture plumbing must not
    /// enqueue on the block under test.
    async fn snapshot(&self) -> calm_server::track_report_read::ReportReadSnapshot {
        calm_server::track_report_read::load_report_read_snapshot(
            self.boot.repo.as_ref(),
            self.boot.report_card_id.as_str(),
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .expect("snapshot")
    }

    async fn doc_rev(&self) -> u64 {
        self.snapshot().await.doc_rev
    }

    /// Create one `chart.series` block through the real upsert tool.
    pub async fn write_series_block(&self, payload: Value) -> String {
        let if_doc_rev = self.doc_rev().await;
        let out = call_tool(
            &self.boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&self.boot),
            json!({ "kind": KIND_CHART_SERIES, "payload": payload, "if_doc_rev": if_doc_rev }),
        )
        .await
        .expect("chart.series upsert succeeds");
        out["id"].as_str().expect("upsert returns id").to_string()
    }

    /// Replace an existing block's payload; returns the new rev.
    pub async fn rewrite_series_block(&self, block_id: &str, payload: Value) -> u64 {
        let rev = self
            .snapshot()
            .await
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .map(|b| u64::from(b.rev))
            .expect("block is in the index");
        let out = call_tool(
            &self.boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&self.boot),
            json!({
                "id": block_id, "kind": KIND_CHART_SERIES, "payload": payload, "if_rev": rev
            }),
        )
        .await
        .expect("chart.series rewrite succeeds");
        out["rev"].as_u64().expect("rewrite returns rev")
    }

    /// The real `calm.report.read` as the planner.
    pub async fn read(&self, args: Value) -> Value {
        call_tool(
            &self.boot,
            TOOL_REPORT_READ,
            planner_identity(&self.boot),
            args,
        )
        .await
        .expect("planner can read the report")
    }

    /// `blocks[i].resolved` for one block from a read result.
    pub fn resolved_of<'a>(read: &'a Value, block_id: &str) -> &'a Value {
        read["blocks"]
            .as_array()
            .expect("blocks index")
            .iter()
            .find(|b| b["id"] == json!(block_id))
            .map(|b| &b["resolved"])
            .expect("block is in the index")
    }

    /// The block's current payload, as the resolver would see it.
    pub async fn current_request(&self, block_id: &str) -> SeriesRequest {
        let snapshot = self.snapshot().await;
        let block = snapshot
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .expect("block exists");
        SeriesRequest::from_payload(&block.payload).expect("payload derives")
    }

    // --- resolver driving ---------------------------------------------------

    /// `enqueue` for a block, deriving the request from its current payload.
    pub async fn enqueue(&self, block_id: &str) -> Enqueue {
        let request = self.current_request(block_id).await;
        self.resolver()
            .enqueue(self.ctx(), self.track_id(), block_id, &request)
            .await
    }

    /// Take every recorded job (unstarted mode) and run it; outcomes in
    /// order.
    pub async fn run_recorded_jobs(&self) -> Vec<ResolveOutcome> {
        let jobs: Vec<Job> = self.resolver().take_recorded_jobs();
        let mut outcomes = Vec::with_capacity(jobs.len());
        for job in jobs {
            outcomes.push(self.resolver().resolve(job).await);
        }
        outcomes
    }

    /// `enqueue` + run the recorded jobs, for tests that just want "resolve
    /// this block now".
    pub async fn resolve_block(&self, block_id: &str) -> (Enqueue, Vec<ResolveOutcome>) {
        let enqueued = self.enqueue(block_id).await;
        let outcomes = self.run_recorded_jobs().await;
        (enqueued, outcomes)
    }

    // --- rows ---------------------------------------------------------------

    /// Every `report_series` row of the fixture track, ordered by
    /// `(block_id, request_hash)`.
    pub async fn rows(&self) -> Vec<Row> {
        rows_for(self.boot.repo.as_ref(), self.track_id()).await
    }

    pub async fn row(&self, block_id: &str) -> Option<Row> {
        let request = self.current_request(block_id).await;
        self.rows()
            .await
            .into_iter()
            .find(|row| row.block_id == block_id && row.request_hash == request.request_hash)
    }

    pub async fn wait_for_row(&self, block_id: &str, within: Duration) -> Row {
        let deadline = Instant::now() + within;
        loop {
            if let Some(row) = self.row(block_id).await {
                return row;
            }
            assert!(Instant::now() < deadline, "no row landed for {block_id}");
            sleep(Duration::from_millis(20)).await;
        }
    }
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub(crate) struct Row {
    pub block_id: String,
    pub request_hash: String,
    pub status: String,
    pub reason: Option<String>,
    pub as_of: String,
    pub resolved_at: i64,
    pub pinned: bool,
    pub summary: Option<String>,
    pub data: Option<String>,
}

impl Row {
    pub fn summary_json(&self) -> Value {
        self.summary
            .as_deref()
            .map(|s| serde_json::from_str(s).expect("summary is JSON"))
            .unwrap_or(Value::Null)
    }
    pub fn data_json(&self) -> Value {
        self.data
            .as_deref()
            .map(|s| serde_json::from_str(s).expect("data is JSON"))
            .unwrap_or(Value::Null)
    }
}

/// Poll `done` until it holds, panicking past `within`. For waiting on an
/// event a seam exposes (a failpoint counter, a call count), never as a
/// stand-in for ordering two tasks by delay.
pub(crate) async fn wait_until(what: &str, within: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(10)).await;
    }
}

pub(crate) async fn rows_for(repo: &dyn Repo, track_id: &str) -> Vec<Row> {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_as::<_, Row>(concat!(
        "SELECT block_id, request_hash, status, reason, as_of, resolved_at, pinned, ",
        "summary, data FROM report_series WHERE track_id = ?1 ",
        "ORDER BY block_id, request_hash"
    ))
    .bind(track_id)
    .fetch_all(&pool)
    .await
    .expect("select report_series")
}

pub(crate) fn write_program(control_dir: &Path, program: Value) {
    let path = control_dir.join("reply.json");
    let tmp = control_dir.join("reply.json.tmp");
    std::fs::write(&tmp, program.to_string()).expect("write reply program");
    std::fs::rename(&tmp, &path).expect("rename reply program");
}

pub(crate) fn read_calls(control_dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(control_dir.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("call line is JSON"))
        .collect()
}

/// The A4 seam fixture (`tests/fixtures/market_series_reply.json`).
pub(crate) fn seam_fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/market_series_reply.json"))
        .expect("market_series_reply.json parses")
}

/// UTC midnight of a `YYYY-MM-DD`, in milliseconds.
pub(crate) fn midnight_ms(date: &str) -> i64 {
    let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("date");
    date.and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

/// One `ok` series entry with `[date, value]` points.
pub(crate) fn ok_series(asset: &str, complete_through: &str, points: &[(&str, f64)]) -> Value {
    let points: Vec<Value> = points
        .iter()
        .map(|(date, value)| json!([midnight_ms(date), value]))
        .collect();
    json!({
        "asset": asset,
        "currency": "USD",
        "status": "ok",
        "complete_through": complete_through,
        "points": points,
    })
}
