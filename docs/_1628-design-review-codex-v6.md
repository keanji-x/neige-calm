Reviewed read-only against `c534bf6b`. HEAD differs only in design/review documents; worktree clean; `git diff --check c534bf6b HEAD` passed. Current `origin/main` is `f8877ae2`.

**Round-5 adopted-disposition audit — resolved by specified mechanisms:**

- R5-1: D2/D3 and S2.7 share `row_ttl`; any non-`ok` series selects two minutes, preserving successful series.
- R5-3: explicit request `mode`, derived from payload `as_of` presence; frozen-yesterday cannot masquerade as live.
- R5-2: S3.4 keys pages by fetch UTC date and retains minimum pre-fetch watermark across reused pages.
- S3.5 relaxes only `live ∧ day ∧ non-crypto`; crypto, week/month and frozen remain strict. US additionally has the U9 fallback prerequisite.
- S3.3 requires same-source probe/fetch and re-probing after fallback.
- S2.9 branches kernel validation on `(mode, period)`; crypto-specific enforcement deliberately remains plugin-side.
- Remaining adopted constraints are explicit: current-hash full-primary-key lookup; atomic in-flight insertion; optional SQLite-pool handling; summary-only SELECT; period-end comparisons and live weekly negatives; rev-keyed frontend queries without series-prefix invalidation.
- D6 scope wording and §11 disposition status are corrected. S1.x–S4.x are numbered; §9 retains S3 spike prerequisites. U9 is deferred verification, not established provider finality.

**Matrix constructions** (`C` = effective minimum pre-fetch `complete_through`):

| Construction | Result |
|---|---|
| US/HK/SH/SZ, live/day, fresh fetch, yesterday’s bar equals C | Included; live remains unpinned. Correct under the stated source-finality assumption. |
| Same asset/window, frozen/day | Equality is excluded; pinning requires C strictly beyond cutoff. Mode distinction survives. |
| Crypto/live/day, host seconds ahead at midnight, source still on D | Cutoff D, C=D: D excluded. Same-day cached partial D also excluded. |
| Crypto/live/day, host behind midnight | Earlier cutoff omits a completed bar conservatively. |
| Any venue, frozen/day, cached partial D followed by newer probe | Cached watermark lowers C to D; D cannot be certified or pinned. |
| Any venue/mode, yesterday’s cache page reused after midnight | Date-key mismatch forces fetch; previous-day bytes cannot acquire today’s provenance. |
| Any venue, live/week, Monday 00:30 with Friday missing | Sunday end ≥ C: previous week excluded. Month-end construction behaves identically. |
| Any venue, frozen/week or month, future cutoff | Current partial period fails end < C, even though end ≤ cutoff. |
| Live DB row aged two minutes across midnight | Remains fresh under six-hour TTL. Explicitly accepted staleness; page-cache expiry does not expire stored rows. |
| Partial-success row aged three minutes versus all-success row | First retries; second remains fresh. Original R5-1 construction is closed. |

**MINOR — doc:** §2.5, D2 and G21 still claim “错误存活 ≤ 6h” / “6h 自愈”. Six hours only enables refresh; reads, lane delay and a subsequent frontend fetch remain necessary. Replace with “eligible for refresh after six hours”; §2.8/G12 already state the correct limitation.

**MINOR — implementation-brief:** S3.1/U9 should explicitly treat inconclusive measurements as strict-US mode. Two unchanged samples do not establish exclusion of extended-hours trades. Verify both sources independently; the relevant late session reaches 20:00 ET. [NYSE hours](https://www.nyse.com/trade/hours-calendars)

**MINOR — doc:** State the clock assumption behind relaxed equities. Construction: winter US session still open at actual 20:30 UTC, host four hours fast → host-yesterday includes the unfinished bar. Seconds-scale skew fits the stated margins; arbitrary clock jumps do not. This limits the guarantee rather than requiring a new clock subsystem.

Touched `[实测]` checks reproduced: venue parsing; `sqlite_pool()` default/forwarding; report-event payload and invalidation plan; ifzq `n=3` response keys and US baseline row; Binance’s latest candle had future `closeTime` at 04:10 UTC. Binance’s documented default timezone remains UTC. [Binance contract](https://developers.binance.com/en/docs/catalog/core-trading-spot-trading/api/rest-api/market) No code tests or mutation runs were performed.

**Nothing remains at MAJOR or above.** Approval is for the design with its existing S3 spike prerequisites; the findings above are non-blocking.

APPROVE