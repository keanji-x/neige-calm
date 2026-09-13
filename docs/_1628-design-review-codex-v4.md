Read-only review against `c534bf6b`; production code matches that baseline and the worktree remains unchanged. `D:L` means [1628-report-chart-series.md](/mnt/data2/kenji/neige-calm/.claude/worktrees/review-b-1628/docs/architecture/1628-report-chart-series.md):L. Network spike results were not rerun.

Round-3 adopted-disposition audit:

- **R3-2 / A M1 / orchestrator window change:** concrete mechanism at D:131–134,242,275–280,398,430: explicit window, pagination, separate probe and depth failure. Approved `start` replaces wire `range` consistently across D1/D2/request/validation/S3. Keeping payload `range` for hashing and point caps is consistent.
- **R3-1 / A M2 / strict-`>` disposition:** implemented in the proposed contract at D:272,284,324,417, including daily-only progress and live never pinning. Original equality construction closes; the separate-fetch race below remains.
- **R3-3:** calendar aggregation and kernel checks at D:133,272 close the original Wednesday-cutoff construction. Future cutoffs remain defective below.
- **R3-4:** D:264,267–268,423 explicitly retain the negative outcome and prohibit calls. Verified independently sampled runtime state at `crates/calm-server/src/plugin_host/mod.rs:3260,3312`.
- **R3-5 / A m1:** unbounded send and locked reconstruction at D:265 close capacity waiting and concurrent reconstruction. Verified receiver draining in local Tokio 1.52.3 `sync/mpsc/chan.rs:487–512`. The claimed queue bound is false below.
- **R3-MINOR-1:** D:167–168,323–324 now scope the timeout, enumerate failures and require TTL plus another read; these are explicit contract limitations.
- **R3-MINOR-2:** valid `aa` fixture and meaningful assertions at D:427; verified minimum ID length at `plugin_host/manifest.rs:2305`.
- **R3-MINOR-3:** pre-S3 `NotExposed`, zero calls at D:401; verified `plugins/market/manifest.json:10–62` and unreachable unknown-tool branch at `main.rs:2572`.
- **A m2:** scope restriction and gap recorded at D:269,457; verified `mcp_server/tool_visibility.rs:141`, template admission at `routes/tracks.rs:1683,1748`, and `plugins/git-forge/manifest.json:302`.
- **A m3:** nullable summary at D:314 resolves unavailable-row storage.
- **A m4:** transactional block snapshot at D:267 is concrete; verified `track_report.rs:73`, heavier read at `track_report_read.rs:50`, and snapshot callers `task_recovery/admission.rs:158`, `file_delivery/repair.rs:101`.
- **A m5:** injected timeout and unstarted recorder at D:265,413,426 replace timing-based negative checks.
- **A m6:** reran the targeted event search: 26 occurrences; production constructor verified at `track_report/write.rs:1098`.
- **A m7:** D:206,321,418 explicitly update and assert live `as_of`.
- **Orchestrator spike adoption:** operational constraints are incorporated at D:131–134,398,430; this verifies incorporation, not the external measurements.
- **R3-6:** explicitly deferred by scope decision, not technically resolved; verified unbounded accumulation at `plugin_host/mcp.rs:783` before parsing at :802. Not reopened here.

Remaining findings:

- **MAJOR — A later probe can certify an earlier, unfinished window.** D:131–132,324.
  Construction: fetch the cutoff-day window at `2026-09-13 23:59:59`, retaining its changing daily bar. Fetch the separate latest-bar probe at `2026-09-14 00:00:01`; it reports September 14. Every check passes and strict `>` permanently pins the pre-close value. Correct dates and chronological publication both hold; no historical correction is needed. Pagination extends this race window.
  Require a progress observation **before** the data fetch it certifies, or refetch affected data after observing progress. Specify how cached pages obey that ordering.

- **MAJOR — Future cutoffs still emit unfinished weekly/monthly bars.** D:133,232–233,241,272.
  Construction: on Wednesday September 9, request weekly data through Sunday September 13. Aggregate Monday–Wednesday into the September 7 weekly point. Its period end is ≤ cutoff, its timestamp is Monday, and `complete_through=September 9` is ≥ its timestamp. With preceding weeks, every validation passes. The row stays unpinned but displays the prohibited partial week. A future month-end cutoff admits the current partial month identically.
  Completion must account for observed time/source progress as well as the requested cutoff.

- **MAJOR — In-flight deduplication does not bound queues by current block count.** D:246,263,265,267.
  Construction: hold the lane behind a slow call; repeatedly edit and read one block with distinct `series` or `as_of` values. Each creates a different `(track,block,hash)` key. One current block can retain thousands of queued jobs. Stale-hash rejection occurs only after dequeue, so it does not prevent accumulation; deleting the block likewise leaves queued jobs.
  Bound/coalesce pending work by block, including superseded hashes, or introduce explicit nonblocking admission limits. This is separate from the deferred transport allocation issue.

- **MINOR — A9f overstates its concurrency proof.** D:265,424.
  `&mut HashMap` can come from an ordinary local map; the signature does not prove mutex ownership. Also, 200 probabilistic rounds cannot guarantee the mutation appears. Keep the actual locked implementation, but use deterministic synchronization to exercise competing reconstruction attempts and remove the “必现” claim.

REVISE