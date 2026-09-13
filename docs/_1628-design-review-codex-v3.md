Reviewed read-only against `c534bf6b`; worktree unchanged. Current `origin/main` is `bd033633`, but production code here matches the requested baseline. `D:L` below means `docs/architecture/1628-report-chart-series.md:L`.

Round-2 disposition audit, before new constructions:

- **Resolved in the proposed design:** R2-3 source identity; R2-4 subscriber removal; R2-5 drain-side TTL; R2-9 minimum points; R2-MINOR/view equivalence. Concrete rules appear at D:223,235,244,249.
- **R2-1:** responder cleanup has a viable mechanism at D:248. Verified the synchronous responder map and cancellation gap at `crates/calm-server/src/plugin_host/mcp.rs:233,667,676,804,867`. Plugin cancellation remains explicitly unresolved.
- **R2-2:** deferred, not resolved; see finding below.
- **R2-6:** subscriber loss disappears, while unattended resolution/freshness is withdrawn at D:238,419,427. Remaining contradictory guarantees are findings below.
- **R2-7 / A m10:** immutable-row equality is now supported by D:298; live equality is explicitly withdrawn at D:303.
- **R2-8:** transactional copying now covers all rows at D:304, supported by `crates/calm-server/src/routes/tracks.rs:2322,2730`. Non-trading-day handling still depends on the defective completeness model below.
- **A M1:** unresolved completeness claim. **A M2:** pure shape validation resolves the clock-boundary issue; checked `crates/calm-types/Cargo.toml:18` and its source clock searches. **A M3:** yesterday UTC fixes daily live inclusion, but period aggregation remains unspecified. **A M4:** guard ownership and lane reconstruction address the original fail-lock constructions at D:240,242.
- **A m1/m2/m7:** covered by view identity, responder guard, and minimum points above. **A m3:** exact lookup works with `registry.rs:292`, `transport.rs:771`, and `manifest.rs:2303`; its fixture needs correction below.
- **A m4:** reason truncation is specified for both block and series failures at D:249,287,412. **A m5:** subscriber removal eliminates the trigger distinction. **A m6:** persistent nonexistent-plugin lanes disappear, but the replacement creates a concurrency race below.
- **A m8/m9/m11:** scope reasons, oracle ownership, and UTC-zero timestamps are concretely addressed at D:246,167,249; scope behavior verified at `crates/calm-server/src/mcp_server/tool_visibility.rs:138`.
- **Orchestrator dispositions:** read-only triggering and slice ownership are reflected at D:235,374. Line estimates are estimates, not verified implementation results. The event-bus crate correction matches `crates/calm-truth/src/event_bus.rs:185`.

Findings:

- **MAJOR — Equality does not prove the cutoff bar is closed; U7 remains open.** D:261,301,447. Construction: request frozen cutoff today during trading; the source returns today’s changing bar and `complete_through=today`. Every prescribed check passes, permanently pinning an intraday value. Recording U7 does not resolve adopted A M1/M2.
  **Strict `complete_through > as_of` closes this specific daily-bar construction**, assuming correctly dated, chronologically published source bars. It delays ordinary trading-day pinning until a later dated bar appears; a terminal/delisted series can remain unpinned forever. Weekend cutoffs still pin on the next later bar. It does not prove historical completeness or prevent later source corrections.

- **MAJOR — “Latest N, then filter” can permanently pin a truncated historical window.** D:115,249,261,301. Construction: resolve `range=1Y` with a cutoff six months ago using the latest year of bars. Filtering leaves roughly half the requested historical year; it still has ≥2 points, satisfies the maximum count, and has `complete_through > as_of`. The truncated chart pins permanently even with strict comparison. Older cutoffs eventually produce no data despite source history existing. Define the window relative to the cutoff and fetch/paginate that window; the latest observation alone is insufficient.

- **MAJOR — Weekly/monthly aggregation can reintroduce incomplete bars.** D:146,252,375,425. Construction: on Thursday, freeze through Wednesday and aggregate the cutoff-filtered daily rows into a weekly candle. Monday–Wednesday becomes a partial weekly candle; Thursday’s unfiltered date satisfies even strict `>`. Alternatively, live aggregation can label the current partial week with its latest available date, passing yesterday’s cutoff. Specify aggregation order, period boundaries, and exclusion of unfinished periods. Strict daily comparison alone does not settle G10’s “bar closing date” contract.

- **MAJOR — Route-precheck misses can bypass the serial lane.** D:241,245. Construction: while a registered plugin is stopped, enqueue many distinct blocks; each gets a standalone `resolve` task. Start the plugin before those tasks perform their second lookup. They now find it running and all invoke it outside its lane. Verified running status is independently sampled at `crates/calm-server/src/plugin_host/mod.rs:3260,3312`. Per-key guards do not serialize distinct keys. A miss task must either retain a negative outcome or enter the lane before making a call.

- **MAJOR — Bounded lane admission can put plugin delay back into reads.** D:242 specifies `mpsc::Sender<Job>` and `send`, while D:337 promises enqueue adds no plugin waiting. Construction: fill the bounded channel behind a hung 30-second call; the next read awaits channel capacity. Larger reports can wait repeatedly. Specify nonblocking admission and the full-queue outcome; merely spawning a drain does not establish a nonblocking read contract.

- **MAJOR — R2-2 remains a wording/scope disposition, not a resource fix.** D:249,426. Construction still succeeds: plugin stdout emits an arbitrarily long line without newline. Verified `crates/calm-server/src/plugin_host/mcp.rs:783` accumulates it before parsing at :802. The responder guard, timeout, reason cap, and reply acceptance cap cannot bound that allocation. V3 accurately acknowledges this, but the original defect remains deferred to #1634.

- **MINOR — Read-triggered guarantees still contradict the failure and freshness rules.** D:148 promises a stored row within 30 seconds after dequeue, but the timeout covers only the plugin call; D:250 permits write failure without a row, and D:240 allows queued jobs to disappear on lane panic. D:183,301 promise pinning by the next trading day despite requiring another read after TTL. D:300 promises refresh after crossing midnight, but a row written at 23:59 remains fresh at 00:01. These need explicit exceptions or additional mechanisms.

- **MINOR — A11’s routing regression fixture cannot pass normal manifest validation.** D:402 registers plugin `a`; verified `crates/calm-server/src/plugin_host/manifest.rs:2305` rejects IDs shorter than two characters. Use a valid fixture such as `aa`, tool `b_c`, and source `aa_b/c`; otherwise the proposed production-entry regression cannot reach its routing assertion.

- **MINOR — The pre-S3 behavior still names the wrong failure path.** D:378 predicts plugin `unknown tool`. Baseline `plugins/market/manifest.json:10` does not expose `market.series`, so D:245 rejects it as `NotExposed` before calling the plugin. The actual unknown-tool branch exists at `plugins/market/main.rs:2572`, but this construction cannot reach it.

REVISE