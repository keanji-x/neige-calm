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
- `context?: InvalidationContext` — `findTrackOwningCard`, so card-scoped events
  can reach their track. Absent it, card-derived track keys are simply skipped.

## Event kind → query key

The reducer emits *planned* keys in legacy shapes; the adapter maps them onto
`queryKeys` from `app/providers/queries.ts` and drops anything with no query
behind it. Mapping table (`mapPlannedQueryKey`):

| Planned key | Mapped key | Note |
| --- | --- | --- |
| `['areas']` | `queryKeys.areas()` | |
| `['tracks','area',id]` | `queryKeys.tracksInArea(id)` = `['tracks', id]` | shape differs; this is the only place that knows |
| `['track', id]` | `queryKeys.trackDetail(id)` | |
| `['overlays','track'\|'card']` | `queryKeys.overlaysByKind(kind)` | |
| `['harness-items', cardId]` | `queryKeys.harnessItems(cardId)` | fe conversation history is query-backed |
| `['planner-run', cardId]` | `queryKeys.plannerRun(cardId)` | fe current harness phase is query-backed |
| `['track-files', …]` | — | **no-op**: no track-files query is built yet (stub) |
| `['track-report', id]` | `queryKeys.trackReport(id)` | the track's task verdicts (TASKS panel) |
| `['track-report']` | `queryKeys.trackReportPrefix()` | prefix; the four `task.*` events carry no track-id *field* (it is embedded in `idempotency_key`, which the plan does not parse), so this is the plan's only key for them |
| `['track-conversations', id]` | `queryKeys.trackConversations(id)` | the endpoint is per-track (#1189 §4.1) and the plan names the track whenever `derivedTrackId` resolves one |
| `['track-conversations']` | `queryKeys.trackConversationsPrefix()` | fallback for a `runtime.*` event whose card belongs to no cached track detail. The query behind both arities lands in S5; mapping first is harmless (invalidating an unmounted key is a no-op) and the reverse order is what silently breaks a list |
| `['tracks-range']` | — | **no-op**: the calendar range query is not built yet (stub) |
| `['track-backlinks']` | — | **no-op**: no backlinks query is built yet (stub) |

Resulting per-kind behavior on the currently-built surfaces:

| Event kind | Effect on cache | Reason |
| --- | --- | --- |
| `area.updated` | invalidate areas | area list is live |
| `area.deleted` | invalidate areas + track overlays | |
| `track.updated` | invalidate that area's track list + track detail + all active task verdicts | a root budget/policy change can alter descendant admission; `track-files`/`tracks-range` parts drop (stubs) |
| `track.lifecycle_changed` | invalidate that area's track list + track detail | the route normally pairs it with `track.updated`, but this event alone does not claim a task-policy change; `track-files`/`tracks-range` parts drop (stubs) |
| `track.deleted` | invalidate area's track list + track overlays + all active task verdicts; **remove** track detail | the detail can never resolve again, while deleting a tree member changes the survivors' B/N shares |
| `card.added` / `card.updated` | invalidate track detail + both conversation lists | |
| `card.deleted` | invalidate track detail | knowingly no conversation key on either list; dropping a deleted row is #1140's |
| `runtime.started` / `runtime.status_changed` / `runtime.superseded` | invalidate card overlays, plus track detail when the built-in cache lookup resolves; plus the track's task verdicts and both conversation lists | `track-files` remains an adapter stub. These three are what write `worker_sessions.state`, i.e. the dot each conversation row draws |
| `overlay.set` / `overlay.deleted` | invalidate overlays of that kind, plus the owning track detail | |
| `track.report_edited` | invalidate that track's task verdicts | `track-files` and `track-backlinks` are still stubs |
| `plan.updated` with string `agent_message` | cancel any in-flight initial verdict read, then invalidate that track's task verdicts | standalone pending-task cancellation carries the message and pending rows do not poll; projection companion events omit it and are no-ops here because their paired event already refreshes |
| `codex.hook`, `claude.hook` | invalidate `track-files` **only** | a hook fires ~twice per tool call per worker and writes no `tasks` row; `track-report` is a live whole-document projection, so it is deliberately excluded (`taskVerdictInvalidatingKinds`) |
| `terminal.deleted`, `codex.worker_requested`, `terminal.worker_requested`, `task.dispatched`, `task.completed`, `task.failed`, `task.gate_result` | invalidate task verdicts — by track id when the event resolves one, by prefix otherwise | `track-files` is still a stub. The four `task.*` events carry only an idempotency key / task id, so `derivedTrackId` — which reads named fields, never parsing an opaque id — returns null and only the prefix form reaches the cache: that is why the prefix is mapped at all |
| `harness.item.added` | invalidate harness items | deliberately no conversation key: highest-frequency kind, and it is emitted *before* the `persist_snapshot` that moves the list's ordering column — see the note above `CONVERSATION_LIST_KINDS` in `core/events/invalidation-plan.test.ts` (follow-up tracked in #1216) |
| `harness.phase.changed` | invalidate planner run + both conversation lists | |
| `harness.transcript.cleared` | invalidate harness items + planner run | a reset always emits `harness.phase.changed` too, which carries the lists |
| `harness.user_message.enqueued` | invalidate harness items + planner run + both conversation lists | reset and enqueue cross the transcript/run boundary |
| `plugin.*`, `task.context_*`, `workspace.*`, `forge.*`, `worktree.*`, `review.round`, `ratify.*`, `proposal.*` | **no-op** | `core/events` declares these as `noop(reason)` and no query consumes them |
| unknown / future kind | ignored, no throw | the plan lookup returns an empty plan |

Control frames: `replay-complete` → invalidate everything (`keys: null`) plus a
cursor write; `snapshot-required` → clear the cache, drop the cursor, and
reconnect.

### Effect ownership

- `persist-cursor` and `reconnect` are stream lifecycle; the bridge handles
  them, the adapter ignores them.
- `write-through` (`replace-existing-area`) explicitly converts the wire area
  with `toArea`, then replaces only a matching cached row. Missing rows are not
  inserted: the accompanying invalidation must refetch authoritative data.

## Stubs, stated plainly

The legacy app maps 40+ kinds. This slice covers only the kinds that keep the
built surfaces live — areas, tracks in an area, track detail, track overlays.
Everything routed to `track-files`, `tracks-range` or `track-backlinks` is a stub
here and becomes real the moment those queries exist: the mapping is one entry
in `mapPlannedQueryKey`, and the pure plan already emits the key. `track-report`
was such a stub and is now real — `trackTaskVerdictsQueryOptions` in
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
in `core/domain/report.ts` for the accounting and `trackTaskVerdictsQueryOptions`
for the measured interval.
