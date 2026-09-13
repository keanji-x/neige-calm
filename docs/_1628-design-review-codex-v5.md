Reviewed read-only against `c534bf6b`; HEAD differs only in review/design documents. Current `origin/main` is `f8877ae2`. Worktree remains clean. **D:L** refers to [the v5 design](/mnt/data2/kenji/neige-calm/.claude/worktrees/review-b-1628/docs/architecture/1628-report-chart-series.md).

**Round-4 disposition audit**

- **R4-1 resolved:** D:134–135 requires probe-before-fetch across every page; cached pages retain their earlier probe and lower the reply watermark. The original late-probe certification construction is rejected.
- **R4-2 resolved:** D:136,278 applies `period_end ≤ as_of ∧ period_end < complete_through` in both aggregation and kernel validation. Future-cutoff partial weeks/months are excluded.
- **R4-3 resolved structurally:** D:269–273 coalesces by `(track,block)` and derives the current request after dequeue. Atomic admission needs the implementation constraint below.
- **A M1 resolved:** D:238–248,273,435 specifies calendar validation, rejection tests and defensive drain handling.
- **A M2 partially resolved:** D:270,274–276 makes precheck/scope/client misses discard-only; D:273,329 supplies a two-minute unavailable-row TTL. Per-series failures escape that TTL; see MAJOR below.
- **A m6 read admission resolved:** D:273 explicitly uses a short DEFERRED read transaction, released before calls. Verified `state.rs:686`, `track_report.rs:73`, `events.rs:872`, `infra.rs:15`, WAL configuration and SQLx’s default `BEGIN`.
- **Remaining adopted MINORs have concrete dispositions:** A9f deterministic lock observation; restart loss exception; `1M/month` rejection; U8 spike prerequisite plus near-end check; three-row probe with non-baseline requirement; poison recovery; 409 waiting; global-lane delay disclosure. U8 remains an implementation prerequisite, not a completed measurement.

**LIVE proposal — venue verdict**

| Venue | Verdict for freshly fetched daily bars |
|---|---|
| US equities / ifzq | **Conditional.** Regular-session close is 20:00/21:00 UTC, safely before midnight. Extended trading reaches 00:00/01:00 UTC. The premise therefore requires evidence that ifzq’s daily bar represents the regular session; exchange hours alone do not establish the provider’s contract. [NYSE hours](https://www.nyse.com/trade/trading-information) |
| HK equities / ifzq | **Calendar cutoff is sound:** closing auction ends by 08:10 UTC. Provider publication/finality still must not be inferred solely from that clock. [HKEX hours](https://www.hkex.com.hk/Services/Trading-hours-and-Severe-Weather-Arrangements/Trading-Hours/Securities-Market) |
| CN equities / ifzq | **Calendar cutoff is sound:** ordinary equity closing auction ends at 07:00 UTC, well before midnight. Same provider-finality qualification. [SSE rules](https://www.sse.com.cn/lawandrules/sselawsrules2025/stocks/exchange/c/c_20260424_10816482.shtml) |
| Binance crypto | **Sound for UTC `1d` bars fetched after the boundary.** Retain default/explicit `timeZone=0`; the API exposes close time. [Binance contract](https://developers.binance.com/docs/binance-spot-api-docs/rest-api/market-data-endpoints) |

I reran public GETs for `usNVDA/hk09988/sh600519/sz000001`, `n=3`: US returned the 2011 baseline plus September 11; HK returned `day`, CN `qfqday`. Binance returned a latest bar whose close time was still future. These reproduce the touched probe measurements; **they do not prove ifzq’s intraday/session-finalization behavior**.

**BLOCKER — none found.**

**MAJOR — Per-series transient failures retain the six-hour freeze.** D:193–194,278,329,442.
Construction: a two-asset reply contains one valid series and one `status:"unavailable"` after a transient upstream failure. The envelope passes validation, becomes block-level `ok,pinned=false`, and receives the six-hour TTL. The source recovers immediately; repeated reads still cannot retry that missing series after two minutes. This is the sibling branch of adopted A M2. Select retry TTL from series outcomes as well as block status, preserving successful data; test the production per-series-error path.

**MAJOR — The proposed LIVE relaxation reopens R4-1 through cache reuse.** D:135–136.
Construction: a frozen/direct request caches day D at 23:59 with a partial Binance bar and `observed_complete_through=D`. After midnight, LIVE cutoff becomes D and reuses that page. Current v5 excludes D; proposed `date ≤ cutoff` admits its cached intraday value. Later wall-clock time does not finalize previously fetched bytes.
Require post-close cache provenance or refetch uncertified pages. Keep FROZEN’s existing probe ordering and strict inclusion/pinning rules.

**MAJOR — LIVE/FROZEN branching lacks a wire discriminator.** D:282–289.
Both currently send identical `{start,as_of,…}` requests. A frozen cutoff of yesterday is indistinguishable from LIVE, yet the proposal demands different plugin inclusion rules. Add an explicit completion policy/mode and define its kernel validation and cache behavior; inferring mode from the cutoff would weaken FROZEN.

**MINOR — Period-end semantics must survive the relaxation. [Implementation-brief constraint]**
With `period_end ≤ yesterday_utc`, LIVE cannot include the current partial week/month. If “date” instead means the stored Monday/month-first timestamp, Wednesday’s LIVE request admits Monday–Tuesday as a weekly candle. Retain `period_start ≥ start` and **period_end** cutoff checks in both layers; add weekly/monthly LIVE negatives.

**MINOR — Admission must select the current hash. [Implementation-brief constraint]** D:273,331,366.
Old rows deliberately remain. After h1 pins and the block changes to h2, a `(track,block)` lookup can find h1 and suppress h2 forever. D3’s identity implies the fix: derive h2, then select exactly `(track,block,h2)` before checking TTL/pinning. Test retained h1 alongside missing h2.

**MINOR — Make in-flight insertion atomic after precheck. [Implementation-brief constraint]** D:269–271.
Two readers can both pass the precheck-before-insertion gap. Require locked `HashSet::insert` and create a guard only for the successful inserter. Also correct `inflight.len() ≤ not-yet-dequeued blocks`: executing jobs retain guards too.

**MAJOR findings remain; the LIVE proposal is not sound as a drop-in rule deletion.**

REVISE