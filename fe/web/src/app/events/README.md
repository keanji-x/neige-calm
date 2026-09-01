# App events glue

React/TanStack assembly for the server event stream. Nothing here decides *what*
a server event means; it only decides how an already-planned effect becomes a
cache call and who owns the stream lifecycle.

## Three layers

| Layer | Module | Knows about | Never touches |
| --- | --- | --- | --- |
| pure | `core/events/{protocol,reducer,invalidation-plan}.ts` | frame decoding, cursor arithmetic, which keys an event makes stale | `QueryClient`, React, WebSocket, storage |
| platform | `web/src/systems/events/event-stream.ts` | the typestate + driver port (`configure` → `start`/`stop`) | query keys, cache, React |
| app glue | `web/src/app/events/*` (this directory) | `QueryClient`, React effects, cursor port | frame decoding, invalidation policy |

`core/events/invalidation-plan.ts` deliberately does not import `QueryClient`;
`query-invalidation-adapter.ts` is the only module in the tree that does the
translation, and `event-bridge.tsx` is the only module that owns a stream.

### `event-stream.ts` surface consumed here

`UnconfiguredEventStream` exposes `on`, `onFrame`, `onConnectionState` and a
single `configure({ syncEventVersion, topics })` that returns the
`ConfiguredEventStream` handle with `start()` / `stop()`. The bridge registers
`onFrame` *before* `start()` so the first frame after connect cannot be lost.

## Invariants

- **INV-APP-001** — the bridge mounts **inside** `ServerCompatGate`, through its
  `renderEventBridge(syncEventVersion)` prop. Locked by rendering the real gate
  and asserting the bridge marker (`data-nc-event-bridge`) is absent while the
  version query is pending and present in the gate's subtree afterwards —
  position, not just presence.
- **INV-APP-020** — the bridge is the only `start()` caller. Types cannot prove
  "only one", so the contract test re-renders with fresh prop identities,
  unmounts, and mounts a second bridge over a second stream, asserting exactly
  one `start()` per stream instance. This is why the stream is a prop: a module
  singleton would let any importer start it.
- **INV-APP-021** — `configure()` must not connect. The fake stream's
  `configure` asserts that no connect has happened at that moment, and the test
  then asserts `start()` connects exactly once.
- **GATE-APP-079** — this slice ships **no** dev trace buffer. There is
  therefore no `import.meta.env.DEV` short-circuit and no `dev-trace` module
  (none may be created). If tracing is ever added, the `import.meta.env.DEV &&
  …` guard must be written inline at the call site inside the bridge effect so
  the bundler folds the entire right-hand side — buffer functions included —
  into dead code.

## Ports the bridge requires

- `client: QueryClient` — injected, never constructed here.
- `stream: UnconfiguredEventStream` — created and owned by the caller.
- `cursor: SyncCursorPort` — `read()`/`write()`. The implementation must persist
  under `SYNC_CURSOR_KEY` (`calm:sync:cursor`) from `core/keys/storage.ts`; the
  bridge never calls `localStorage` itself.
- `context?: InvalidationContext` — `findWaveOwningCard`, so card-scoped events
  can reach their wave. Absent it, card-derived wave keys are simply skipped.

## Event kind → query key

The reducer emits *planned* keys in legacy shapes; the adapter maps them onto
`queryKeys` from `app/providers/queries.ts` and drops anything with no query
behind it. Mapping table (`mapPlannedQueryKey`):

| Planned key | Mapped key | Note |
| --- | --- | --- |
| `['coves']` | `queryKeys.coves()` | |
| `['waves','cove',id]` | `queryKeys.wavesInCove(id)` = `['waves', id]` | shape differs; this is the only place that knows |
| `['wave', id]` | `queryKeys.waveDetail(id)` | |
| `['overlays','wave'\|'card']` | `queryKeys.overlaysByKind(kind)` | |
| `['harness-items', cardId]` | `queryKeys.harnessItems(cardId)` | fe conversation history is query-backed |
| `['spec-run', cardId]` | `queryKeys.specRun(cardId)` | fe current harness phase is query-backed |
| `['wave-files', …]` | — | **no-op**: no wave-files query is built yet (stub) |
| `['wave-report', id]` | `queryKeys.waveReport(id)` | the wave's task verdicts (TASKS panel) |
| `['wave-report']` | `queryKeys.waveReportPrefix()` | prefix; the four `task.*` events carry no wave-id *field* (it is embedded in `idempotency_key`, which the plan does not parse), so this is the plan's only key for them |
| `['cove-conversations']` | `queryKeys.coveConversationsPrefix()` | prefix only; no conversation-writing event carries a `cove_id` and no cached row can supply one, which is why `queryKeys.coveConversations(id)` keeps the id in second position |
| `['wave-conversations', id]` | `queryKeys.waveConversations(id)` | the endpoint is per-wave (#1189 §4.1) and the plan names the wave whenever `derivedWaveId` resolves one |
| `['wave-conversations']` | `queryKeys.waveConversationsPrefix()` | fallback for a `runtime.*` event whose card belongs to no cached wave detail. The query behind both arities lands in S5; mapping first is harmless (invalidating an unmounted key is a no-op) and the reverse order is what silently breaks a list |
| `['waves-range']` | — | **no-op**: the calendar range query is not built yet (stub) |
| `['wave-backlinks']` | — | **no-op**: no backlinks query is built yet (stub) |

Resulting per-kind behavior on the currently-built surfaces:

| Event kind | Effect on cache | Reason |
| --- | --- | --- |
| `cove.updated` | invalidate coves | cove list is live |
| `cove.deleted` | invalidate coves + wave overlays | |
| `wave.updated` | invalidate that cove's wave list + wave detail | `wave-files`/`waves-range` parts drop (stubs) |
| `wave.lifecycle_changed` | same as `wave.updated` | |
| `wave.deleted` | invalidate cove's wave list + wave overlays; **remove** wave detail | the detail can never resolve again |
| `card.added` / `card.updated` | invalidate wave detail + both conversation lists | |
| `card.deleted` | invalidate wave detail | knowingly no conversation key on either list; dropping a deleted row is #1140's |
| `runtime.started` / `runtime.status_changed` / `runtime.superseded` | invalidate card overlays, plus wave detail when the built-in cache lookup resolves; plus the wave's task verdicts and both conversation lists | `wave-files` remains an adapter stub. These three are what write `worker_sessions.state`, i.e. the dot each conversation row draws |
| `overlay.set` / `overlay.deleted` | invalidate overlays of that kind, plus the owning wave detail | |
| `wave.report_edited` | invalidate that wave's task verdicts | `wave-files` and `wave-backlinks` are still stubs |
| `codex.hook`, `claude.hook` | invalidate `wave-files` **only** | a hook fires ~twice per tool call per worker and writes no `tasks` row; `wave-report` is a live whole-document projection, so it is deliberately excluded (`taskVerdictInvalidatingKinds`) |
| `terminal.deleted`, `codex.worker_requested`, `terminal.worker_requested`, `task.dispatched`, `task.completed`, `task.failed`, `task.gate_result` | invalidate task verdicts — by wave id when the event resolves one, by prefix otherwise | `wave-files` is still a stub. The four `task.*` events carry only an idempotency key / task id, so `derivedWaveId` — which reads named fields, never parsing an opaque id — returns null and only the prefix form reaches the cache: that is why the prefix is mapped at all |
| `harness.item.added` | invalidate harness items | deliberately no conversation key: highest-frequency kind, and it is emitted *before* the `persist_snapshot` that moves the list's ordering column — see the note above `CONVERSATION_LIST_KINDS` in `core/events/invalidation-plan.test.ts` (follow-up tracked in #1216) |
| `harness.phase.changed` | invalidate spec run + both conversation lists | |
| `harness.transcript.cleared` | invalidate harness items + spec run | a reset always emits `harness.phase.changed` too, which carries the lists |
| `harness.user_message.enqueued` | invalidate harness items + spec run + both conversation lists | reset and enqueue cross the transcript/run boundary |
| `plugin.*`, `plan.updated`, `task.context_*`, `workspace.*`, `forge.*`, `worktree.*`, `review.round`, `ratify.*`, `proposal.*` | **no-op** | `core/events` declares these as `noop(reason)` and no query consumes them |
| unknown / future kind | ignored, no throw | the plan lookup returns an empty plan |

Control frames: `replay-complete` → invalidate everything (`keys: null`) plus a
cursor write; `snapshot-required` → clear the cache, drop the cursor, and
reconnect.

### Effect ownership

- `persist-cursor` and `reconnect` are stream lifecycle; the bridge handles
  them, the adapter ignores them.
- `write-through` (`replace-existing-cove`) explicitly converts the wire cove
  with `toCove`, then replaces only a matching cached row. Missing rows are not
  inserted: the accompanying invalidation must refetch authoritative data.

## Stubs, stated plainly

The legacy app maps 40+ kinds. This slice covers only the kinds that keep the
built surfaces live — coves, waves in a cove, wave detail, wave overlays.
Everything routed to `wave-files`, `waves-range` or `wave-backlinks` is a stub
here and becomes real the moment those queries exist: the mapping is one entry
in `mapPlannedQueryKey`, and the pure plan already emits the key. `wave-report`
was such a stub and is now real — `waveTaskVerdictsQueryOptions` in
`app/providers/queries.ts` claims exactly the key the plan already emitted, so
the TASKS panel went live without a line of new invalidation policy.

**Events are not sufficient for that panel, and the gap is on the kernel side.**
`scheduler::mark_running` stamps `worker_card_id` with no event at all —
`task.dispatched` fired before the spawn (column still NULL) and every
`runtime.*` a worker adapter emits is emitted during the spawn, also before the
stamp. Between spawn and completion a terminal worker therefore emits nothing
that reaches this key, and an agent worker emits only hooks, which are excluded
above for cost. The panel closes that window with a bounded refresh timer on the
query itself, not with a new event or a re-included hook; see `hasLiveTaskRun`
in `core/domain/report.ts` for the accounting and `waveTaskVerdictsQueryOptions`
for the measured interval.
