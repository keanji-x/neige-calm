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
| `['wave-files', …]` | — | **no-op**: no wave-files query is built yet (stub) |
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
| `card.added` / `card.updated` / `card.deleted` | invalidate wave detail | |
| `runtime.started` / `runtime.status_changed` / `runtime.superseded` | invalidate card overlays, plus wave detail when `findWaveOwningCard` resolves | |
| `overlay.set` / `overlay.deleted` | invalidate overlays of that kind, plus the owning wave detail | |
| `wave.report_edited` | **no-op here** | its plan is entirely `wave-files` + `wave-backlinks`, both stubs |
| `terminal.deleted`, `codex.hook`, `claude.hook`, `codex.worker_requested`, `terminal.worker_requested`, `task.dispatched`, `task.completed`, `task.failed`, `task.gate_result` | **no-op here** | each plans only `wave-files`, a stub query |
| `harness.*`, `plugin.*`, `workflow.registered`, `plan.updated`, `task.context_*`, `workspace.*`, `forge.*`, `worktree.*`, `review.round`, `ratify.*`, `proposal.*` | **no-op** | `core/events` already declares these as `noop(reason)` — no query consumes them; card-topic report consumers read them directly |
| unknown / future kind | ignored, no throw | the plan lookup returns an empty plan |

Control frames: `replay-complete` → invalidate everything (`keys: null`) plus a
cursor write; `snapshot-required` → clear the cache, drop the cursor, and
reconnect.

### Deliberately unhandled effects

- `persist-cursor` and `reconnect` are stream lifecycle; the bridge handles
  them, the adapter ignores them.
- `write-through` (`replace-existing-cove`) is **ignored**. Its payload is the
  *wire* cove from `core/api/schemas`, while `coveListQueryOptions` caches
  *domain* coves produced by `toCove`; writing the wire row through would
  corrupt the cache. The same `cove.updated` plan already invalidates
  `['coves']`, so the only cost is one refetch. Re-enable it only together with
  an explicit wire → domain conversion.

## Stubs, stated plainly

The legacy app maps 40+ kinds. This slice covers only the kinds that keep the
built surfaces live — coves, waves in a cove, wave detail, wave overlays.
Everything routed to `wave-files`, `waves-range` or `wave-backlinks` is a stub
here and becomes real the moment those queries exist: the mapping is one entry
in `mapPlannedQueryKey`, and the pure plan already emits the key.
