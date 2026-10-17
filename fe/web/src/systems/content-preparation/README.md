# Content preparation leases

`usePreparedContent(selectionKey, input, store, enabled)` coordinates one view's
cancelable preparation. `PreparationStore` supplies only `peek(input)` for a
completed value and `prepare(input, signal)` for explicit execution. The feature
owns its input, algorithm, completed cache and execution capability. Keep the store
identity stable for that owner/capability lifetime; changing it starts a new scope.
This system imports no feature, query, router, parser or business policy.

The result has `waiting`, `preparing`, `ready` or `failed` status, a candidate, an
error channel and retry. A candidate is `{ input, value }`: prepared data only.
It does not prove admission, rendering, a complete DOM commit, visibility or a read.
A same-selection input replacement retains the prior candidate while preparing;
a different selection or store identity drops it immediately. Input/value identity
uses `Object.is`, matching React's dependency semantics. Each attempt owns an abort
controller and exact lease; late results and old retry callbacks cannot retarget a
later visit, including A→B→A. Independent hook instances remain independent.

Disabling aborts/fences execution. An uncached current input becomes waiting and
may retain its same-selection prior candidate. Disabled retry resets local error
without starting work. A completed current-input cache hit may be ready even while
disabled; that is data readiness only. Re-enabling an uncached input starts a fresh
lease. Synchronous results/throws and Promise outcomes use the same result/error
channels. A synchronous algorithm still runs on its caller's thread.

The consumer separately composes UI admission, pending/error/retry surfaces and
actual committed-content evidence. An unrendered cached candidate must never become
a committed fallback merely because the next input begins preparation. Those
view-specific decisions do not belong in this lifecycle. No extra cache, fetch
pipeline, worker, queue, retention knob, feature registry or global context is created.

`public.test.tsx` uses opaque inputs and controlled stores for lifecycle invariants.
Feature adapter tests exercise their real cache, execution and data-association
contracts; production app tests retain ownership of read/restore/commit behavior.
