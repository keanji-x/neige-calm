# UX cycle 6: current cash balances in the portfolio

Design only; base `a48e599c4`. Implementation, new tools and persistence changes
require the next design review/owner decision. Nothing was built, deployed or
written to a user Track for this document.

## Outcome and working assumption

Record “my current cash balance is CNY 100,000” through ordinary chat, retain it
as typed Track data, and include it in the same total, forward history and
allocation as securities. **Including cash is root's stated working assumption;
the user has not answered the optional scope question.** This is a balance
snapshot, not a deposit, trade, execution or investment-return calculation.
Trade logs remain independent reported facts: recording a buy/sell never
implicitly debits cash, and changing cash never invents a transaction. No margin,
negative balances, Barra, broker reconciliation or Save-as-template feature.

## Actual failure and relevant implementation

`cash-observation/first-use/observation.json` records Track
`696677ced6e84ef78e1fccd3664665ef`: the chat completed and its prose survived
reload, but there were zero numeric cash rows and zero Market overlays; the
layout and transaction log stayed unchanged. Its screenshot timeout was a
separate harness heading-selector mistake, not evidence that saving failed.

The source explains the missing capability:

- `plugins/market/main.rs`: `Holding` is only `AssetId + positive quantity`;
  `market.holdings.set` records a security, and quantity zero removes it.
- Holdings live in `holdings/<track_id>`. `portfolios()` enumerates only that
  prefix, and `refresh()` exits as empty when securities are empty.
- `price_holdings`, `PortfolioTotal`, `holdings_table` and `history_table` own
  valuation and its units. Neither renderer nor Report owns current balances.
- `plugins/market/manifest.json` exposes quote/set/list. There is no cash tool.
  The current Track comes only from kernel-injected `_meta[dev.neige/track].id`.
- The canonical `fe/web/src/features/report/recipe/examples/portfolio.md` binds
  the two `portfolio.holdings`/`portfolio.history` overlays. Its allocation and
  share column require complete nonnegative rows and a compatible total.

A new text paragraph or a template-only inline row cannot fix this chain.

## Proposed input and persistence contract

Add two ordinary local App tools, with per-tool `assistant_access: true` under
the existing identity/scope/generation fences. No new host capability; the
existing KV capacity increase below is an explicit owner-approved design decision:

- `market.cash.set {currency, amount}`: replace this Track's **current absolute
  balance** for one currency. Closed world, write, non-destructive annotation;
  only persist and wake the poller. Reply with saved balances and refresh queued,
  never claim a quote/overlay refresh already succeeded.
- `market.cash.list {}`: closed-world, read-only access to saved native balances;
  no pricing/network. A caller can confirm a timed-out write before retrying.

Both schemas forbid extra arguments; neither accepts a Track id. Runtime typed
argument parsing must enforce the same rule. Currency is explicitly `CNY`,
`USD` or `HKD` (normalize case at input); unsupported codes are errors, not a
currency guess. USDT remains the existing crypto holding/settlement-alias
contract, not fiat cash in this first slice. **Owner precision decision:**
`amount` is a required finite JSON number >= 0, representing whole cents for
these three fiat currencies. Accept 0, 0.01, 0.29, 1.20 and 1e2; reject 0.001
and 1.005 with no write. Preserve accepted native amounts; never silently round
an input into an acceptable balance. Reject strings, null, missing, negative,
nonfinite and out-of-range amounts. Validate decimal scale/exponent with checked
cent conversion and safe numeric round-trip bounds; binary `amount * 100` being
exactly integral is not a valid test (it can reject 0.29). Scientific notation,
large-number boundaries and overflow must have explicit acceptance/rejection
fixtures. Repeating an absolute set is safe; there is no additive “deposit”
command or transaction side effect.

New plugin-owned key: `cash/<track_id>` containing
`{version:1, balances:[{currency:"CNY",amount:100000}]}`. One typed, unique entry
per currency, sorted on writes. Missing key means **no cash recorded**, compatible
with old Tracks; explicit `amount:0` remains recorded and visible. Zero does not
remove the row. Removing/forgetting a currency is not in this slice. Unknown
versions, duplicate currencies or malformed persisted balances fail visibly and
are not silently dropped or overwritten by a later set. No DB migration and no
rewrite of holdings KV, old Recipes or old Report bodies.

`portfolios()` must enumerate/deduplicate the union of holdings and cash keys,
including explicit-zero cash documents. A cash-only Track must remain discoverable
after plugin restart, rather than relying on a one-off wake notification.

## One valuation snapshot, explicit projection scopes

Keep the existing `portfolio.holdings` and `portfolio.history` **security-only**
contracts and their `history/<track_id>` key. Adding cash must not silently
change an old template's denominator or splice two different history scopes.
Compute security pricing once through the existing entry; value cash through
`fx_path` directly, never `quote_asset`, `AssetId`, a fake venue or price of 1.
A shared snapshot then derives four new read-only projections:

| Source | Data and purpose |
|---|---|
| `portfolio.allocation` | Security/cash rows with stable `id`, readable `label`, `kind`, settlement `value`/`currency`, plus exactly one `kind:total` row. For example ids `security:SH:510300`, `cash:CNY`, `total`; these are allocation identities, not quote symbols. |
| `portfolio.total_history` | Combined recorded-asset totals, with actual `at`, `total`, `currency`; persist in a new `total_history/<track_id>` key. Never copy or relabel the old security history. |
| `portfolio.positions` | Existing security fields plus a producer-calculated `weight` percentage of the combined total. No cash or Total row. |
| `portfolio.cash` | Native `currency`/`amount`, converted `value`/`value_currency`, actual rate/provenance if obtained, and combined-total `weight`. No fake price, quantity, security or exchange. |

These are derived views, not four balance stores. Only cash KV and holdings KV
own current inputs; the two history keys persist different observations. Keep
`market.holdings.list`'s existing security scope. cash.list reads native saved
balances without networking; combined valuation lives in the projections.

**New projection arithmetic:** quantize each converted item to its public cent
value first, sum those cent values for the new combined Total (checked integer
cent arithmetic to avoid floating accumulation), and compute each position/cash
weight as `100 × item_cents / total_cents`. Allocation rows, positions/cash values,
combined history and Total must share that exact public-value basis. The donut
normalizes displayed row values, so this keeps its ratios equal to the producer
weights. A public Total <=0 means every weight is null, even if the pre-rounding
sum was a small positive number. Overflow or unsafe numeric representation is
unavailable, not wrapped, clamped or silently rounded into a different total.
Do not change the old security-only aggregate algorithm.

Executable small-FX fixtures (synthetic rates, not market claims): USD 0.01 and
HKD 0.01 each converted at fixture rate 0.6 CNY yield raw values 0.006 and 0.006.
Publish 0.01 and 0.01 CNY, new Total 0.02, and weights 50%/50%; do not round raw
sum 0.012 into Total 0.01 and report 100% each. A sole USD 0.01 converted at
fixture rate 0.4 yields 0.004: public value/Total 0.00, null weight and empty
donut, while native cash remains USD 0.01. These cases also pin accumulation
across multiple items and the public-cent history value.

- Existing settlement policy remains: CNY and USD settle; config USDT settles
  as USD under its already disclosed assumption. Do not change install-wide
  quote config in response to one Track's cash request.
- Same-currency cash needs no HTTP or FX request. Different currencies use the
  existing current-pass FX routes/cache/provenance. A missing rate retains native
  balance/currency, leaves converted value/rate unknown and the combined pass
  incomplete. No stale-rate fallback; no combined history point. **New combined
  Total/value currency and all combined weights are null if any recorded asset
  cannot be valued.** Retain available individual values without a subset total.
  The new allocation seed checks the Total row currency, so a null total cannot
  normalize a known subset into 100%. Only old security projections keep their
  existing explicitly partial-total contract. Combined weights require a complete
  finite positive total. Security-only history may advance under its old criteria.
- Unsupported settlement config follows existing native-unit behavior: only
  one common counted currency can total; mixed units cannot be added. Template
  unit mismatch stays unavailable rather than relabelling currency.
- Zero is a recorded balance, not absence or deletion. It requires no FX fetch
  and contributes zero in a supported settlement currency without inventing a
  conversion rate. A zero-only recorded portfolio has labelled Total 0 and may
  record real zero observations. Its weights remain null and the donut empty;
  no divide-by-zero or 100% slice. With no securities and no recorded cash,
  preserve empty/no-history behavior. With securities but no cash record,
  “recorded assets” totals known securities; it does not assert the user's
  unrecorded bank balance is zero.
- Unknown/invalid cash KV is not empty cash. Do not drop malformed rows, guess a
  subtotal as complete, or overwrite them on set. Surface the read/parse error;
  withhold combined numbers/history and publish honest unavailable/null-valued
  combined projections if the host accepts them, so old balances do not appear
  current. Independent old security projections can still obey their contract.
- History is forward-only at actual observation time. No backfill or rewrite.
  Cash changes are portfolio value changes, not measured investment returns.
  Each history remains persist-before-publish; incomplete/unsettlable/nonfinite
  totals contribute no point. A first observation cannot fabricate a line.
- Root's read-only trial-DB measurement (`cycle-06/quota-baseline.json`) found
  Market using 87,584 / 262,144 bytes: three histories held 1,397 points and
  87,270 bytes (about 62 bytes/point); three holdings documents used 314 bytes.
  Two 500-point histories therefore cost roughly 62 KiB per security portfolio;
  four portfolios approach the current 256 KiB ceiling. These are measured
  estimates, not bounds for arbitrary-sized records or unlimited portfolios.
  **Owner-authorized design decision, pending independent review:** explicitly
  raise this plugin's `kv_quota_bytes` to 1,048,576. No new permission category or
  scope; no host quota-policy change, old-data pruning or 500-point retention
  change. Measure actual quota usage with 12 Tracks, two full histories,
  20 securities and three cash currencies each, then force a refusal and confirm
  balances/other Tracks/persisted histories survive. The increased capacity and
  its failure path are part of acceptance, not a deferred capacity TODO.


## Necessary write/publication ordering

Tool calls are serialized by one worker, but `holdings.set` does not hold
`REFRESH_LOCK`. A poll can load old holdings, then finish pricing and publish
following a successful newer set. With cash it can also read a mixed two-KV
snapshot. The sibling holdings.set must participate in the new boundary; this
is not optional and does not need a historical-event or scheduling redesign.

Retain `REFRESH_LOCK` to serialize complete refreshes. Add one short **state /
commit mutex**, shared by holdings.set, cash.set, snapshot reads and publication.
Setters hold only this mutex for read/modify/KV-write, release, then wake/reply.
They never acquire REFRESH_LOCK or make a provider request. Native cash.list
also reads under the state mutex. No new persisted or in-memory revision is
needed: compare the complete typed snapshots themselves.

Refresh holds REFRESH_LOCK across these finite phases:

1. **Capture:** acquire state mutex, read both documents into an immutable typed
   snapshot, release. Preserve the distinction between absent cash and explicit
   zero. No parse-error row is silently discarded.
2. **Price:** quote/convert outside the state mutex. Read both needed history
   documents independently outside it too; REFRESH_LOCK serializes their writers.
   A history read/parse failure is not an empty series. Do not overwrite or
   publish that series; the other series may proceed if its own read and
   valuation criteria pass.
3. **Recheck:** reacquire state mutex and re-read both current documents. If a
   read fails, publish no derived candidate/history. If values differ, discard
   the candidate completely and wake the next pass; no stale partial publish.
4. **Commit:** if values still equal, retain state mutex across every accepted
   current projection and history write/publish. Unavailable/null error
   projections use this same commit barrier too; never release it and then
   publish an obsolete error over a newer successful balance. Each history is stored before
   its overlay. Then release locks. A setter either invalidates a candidate
   before this phase, or writes/ACKs after it. Different old inputs therefore
   cannot publish/append after a newer successful set ACK.

Lock order is refresh mutex → state mutex. Set/list never acquire the former;
no waiting for a poll while holding state. There is still one poller and one tool
worker. No network under state, no unlocked recheck followed by a racy publish,
no “busy; retry” normal user workflow and no automatically repeated model turn.

**ABA:** compare canonical balance/holding values, not operation counts. A→B→A
before recheck is the same current snapshot and may commit. These are current
balances and sampled value history, not a ledger promising an observation of
every intermediate edit. The sample keeps its actual observation timestamp;
it must not claim the intervening B never occurred. An unchanged repeated set
has the same semantics. Restart loses no ordering counter because none exists.

**Failures:** failed/indeterminate KV writes do not claim success or trigger a
compensating overwrite. A callback timeout may follow a committed host write;
cash.list/next refresh re-reads actual KV before any retry. This relies on the
existing per-process FIFO in `plugin_host/mod.rs::spawn_neige_router`: its loop
awaits each `callbacks::dispatch` before handling the next request. A delayed
write/publication is therefore completed before a later read-back or setter
callback can succeed, even when the plugin's 15s reply wait already expired.
Pin that production ordering with a delayed-callback regression: timeout an old
callback, queue read-back/new set, release the old callback, and prove read-back
sees its result and an old success/error projection cannot land after the newer
set ACK. Do not add host CAS or assume timeout cancelled a write. Failed recheck
means no old candidate commit. Where host publication is available, use a visible
unavailable status under the same state/commit lock rather than presenting a
failed-read balance as current.
Failure midway through the six overlays is not an atomic multi-overlay commit:
report which projections failed, preserve valid persisted histories, skip any
unpersisted history overlay, and retry through the next fresh snapshot. Give new
projections a shared observation timestamp so provenance can be checked; do not
claim transactional simultaneous painting by the browser.

**Latency:** REFRESH_LOCK currently spans all HTTP and callbacks. A provider
request has a 10s timeout; sequential distinct quotes plus FX can cost roughly
`10s × (quote requests + FX requests)`, with no portfolio-size/global bound.
Host callback replies use 15s each. Sharing this whole lock with setters would
block the only tool worker behind all provider requests and is rejected.

The state mutex excludes that provider latency but still covers host callbacks:
nominal capture or setter read+write is at most two callbacks (30s in their
reply-timeout envelope); recheck is two, then up to six overlay writes and two
history writes (150s total commit envelope if every callback takes nearly 15s).
Normally these local callbacks should be milliseconds. These are **not strict
end-to-end bounds**: queueing/synchronous stdout writes add cost, and
`plugin_host/mcp.rs::call` has no general stdio tool deadline (10s applies only
to initialize). The existing tool queue may also put a set behind a slow list
or quote. Measure callback lock latency under the actual host during acceptance;
do not disguise it as 15s or introduce a transport/scheduler rewrite. A blocked
provider fixture must prove that cash.set can finish while pricing remains
blocked; a blocked publish fixture must prove the necessary commit ordering.

## Template boundary

Only the canonical **new** Recipe seed selects these new sources. Preserve all
old saved Recipes/Reports and their existing security-only sources. Registering
cash never edits a Report; adopting the cash-aware layout is an explicit new
Recipe/Track or user-requested layout rebuild. Root can explicitly rebuild its
own trial layout for acceptance. Do not silently upgrade all existing Tracks.

Keep three sections and existing native primitives: overview has combined line
and allocation donut, plus a span-2 small cash table; securities table reads
portfolio.positions and formats producer `weight` as percent; transaction log
is unchanged. The complete allocation uses its combined Total and all assets.
Do not make the securities-only table pass the existing “share sums to total”
invariant by weakening it: its denominator now includes cash, so an explicit
producer percentage is the correct field. No extra universal filter, component,
renderer schema, fake security row or portfolio-specific rendering is needed.
The seed remains empty of balances, holdings, private IDs and trading records.

## Reproducible acceptance and affected surfaces

After design approval, start with red tests through the actual Market process
and host-dispatch paths, not a copied valuation helper. Current cash-only set/
listing/refresh is missing; use that as the smallest production red. Extend
`market_plugin_process.rs`/the real market binary fixture, current currency tests
and `mcp_plugin_tools_assistant.rs`; all quote/FX sources in tests are loopback.

| Scenario | Required evidence |
|---|---|
| Chat: current CNY 100,000 | Ordinary Assistant discovers/calls the typed tool; exact cash KV/list value; no Report-prose workaround, Task/trade/holding write or model-side price invention. |
| Cash-only, config CNY | No quote or FX request; new cash/allocation rows and Total 100,000 CNY; 100% cash allocation; two actual poll samples form a flat line. |
| Add security | With fixture 7 units at 4.637 CNY, value 32.46 and combined Total 100,032.46; both weights use this denominator, quote precision unchanged. |
| Set cash 80,000; repeat | Absolute replacement, not subtraction/addition; Total 80,032.46, no duplicate currency, unrelated holdings/log/annotations/layout untouched. |
| Reload/restart, cash only | Same KV and displayed balance; cash key alone rediscovers the Track and resumes history, no empty-holdings early exit. |
| Zero versus missing | Zero persists, shows 0, Total 0 in a known currency, no spurious weight; absent cash with no securities retains old empty semantics. |
| Input/public-cent arithmetic | Accept 0/0.01/0.29/1.20/1e2; reject sub-cent/unsafe inputs without writes. Two converted 0.006 values publish Total 0.02 and 50% each; one 0.004 publishes 0.00 and null weight. |
| Currency failures | Unknown input rejected without writes; current-pass missing FX retains native balance, no fake value/rate or history; mixed unsupported units have no summed total. |
| Races/failures | Pause quote before set; ACK new cash or securities; release old quote: old snapshot cannot publish/append. Delay a real callback past timeout and verify FIFO read-back/ACK ordering. Error projections obey the same commit lock; fail either history read independently without replacing it by []. No false success or unpersisted history. |
| Isolation/compatibility | Missing/spoofed Track, other Track's cash, unopted tool caller, old security-only Track, invalid cash KV, quota failure; no schema/migration or old-template rewrite. |
| Real GUI final pass | New seed → chat cash → numeric row/weight/total → add security → replace cash → refresh. Compare full Report/layout, transaction rows and unrelated Track records before/after. |

Affected implementation (proposed, not authorized yet): Market manifest/README,
new small cash/state modules plus main.rs seams, process/Assistant-dispatch tests,
canonical portfolio seed, focused template/primitive FE tests; no renderer change is proposed.
Kernel permission enforcement, DB migrations, OpenAPI, frozen core API/keys
and renderer schemas should not change. The proposed existing KV quota increase
is owner-approved for design review; no new callback capability is requested. Sweep tool-name/count fixtures for the two opt-ins.
Mutation verification should pin Track isolation, no fake/stale FX/history,
zero-vs-unknown and stale-snapshot discard; then relevant Rust/FE gates and
both independent reviews. Do not run real Codex E2E or allocate a large copied
Cargo target during this design phase. Root owns any later real chat/service use.


## Review decisions before implementation

- Approve the two cash tools, separate KV/source names and explicit-only new seed
  adoption; independently verify the bounded quota increase and refusal coverage.
- Verify the owner-decided whole-cent input rule and public-cent sum/weight
  arithmetic with the stated decimal, exponent, overflow and small-FX fixtures.
- Confirm snapshot-value/ABA semantics and the short commit barrier, including
  no false success after indeterminate writes and measured local callback cost.

## Implementation checkpoint

The owner authorized implementation after both design reviews. Market code is
split into cash parsing/storage, combined valuation and snapshot/publication
modules. The only additional host production support is an owner-approved,
params-free `trace!` inside `callbacks::dispatch` (plugin id and method only),
used to prove the existing FIFO after an actual 15-second callback timeout.
It changes no host dispatch order, authorization, schema or CAS behavior.

The first actual-process and Assistant-host cash tests were red (unknown cash
set tool / absent discovery). The affected Market binary, process and Assistant
surface subsequently ran 91 tests green, including old security assertions,
real Track isolation, blocked provider/commit/error ordering, ABA, restart,
independent history failures and quota refusal. The host capacity fixture uses
12 real Tracks with two 500-point series, 20 securities and three cash balances
each; including its caller it measured 791,785 / 1,048,576 bytes. This measured
fixture is not a promise of unlimited capacity or a bound on arbitrary inputs.
Mutation verification, final gates and independent implementation reviews still
follow this checkpoint; no live service or user data was modified by the author.

Build isolation uses a new `/tmp/neige-ux-cash-target-20260909`, not a copy of any
old target. All Rust commands unset `NEIGE_CODEX_BIN`, set `RUSTC_WRAPPER=`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`,
and cap `CARGO_BUILD_JOBS` at six (or two while independent builders run).
The focused run selects `-p calm-server --bin market --test mcp_integration_suite`
and only Market/Assistant cash tests; broad workspace/real Codex E2E was not run.
