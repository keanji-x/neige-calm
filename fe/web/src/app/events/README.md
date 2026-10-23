# App events glue

React/TanStack assembly for the server event stream. Nothing here decides *what*
a server event means; it only decides how an already-planned effect becomes a
cache call and who owns the stream lifecycle.

## Three layers

| Layer | Module | Knows about | Never touches |
| --- | --- | --- | --- |
| pure | `core/events/{protocol,reducer,invalidation-plan,query-keys}.ts` | frame decoding, cursor arithmetic, which keys an event makes stale | `QueryClient`, React, WebSocket, storage |
| platform | `web/src/systems/events/event-stream.ts` | the typestate + driver port (`configure` → `start`/`stop`) | query keys, cache, React |
| app glue | `web/src/app/events/*` (this directory) | `QueryClient`, React effects, cursor port | frame decoding, invalidation policy |

`core/events/invalidation-plan.ts` deliberately does not import `QueryClient`;
`query-invalidation-adapter.ts` executes its already-addressed cache effects,
and `event-bridge.tsx` is the only module that owns a stream.

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

`core/events/query-keys.ts` owns the named address factories for
resources reachable by events. The plan uses these factories and app
`providers/query-keys.ts` references the same functions alongside app-only keys.
The adapter passes planned keys straight to invalidate/remove/reset; there is
no translation table or silent rejection of an unknown address.

| Resource | Shared address |
| --- | --- |
| areas / tracks in area / track detail | `['areas']` / `['tracks', areaId]` / `['track', trackId]` |
| overlays | `['overlays', 'track' or 'card']` |
| task verdicts / backlinks | `['track-report', trackId]` / `['track-backlinks', trackId]`, plus their bare prefixes |
| conversation list | `['track-conversations', trackId]`, or its bare prefix when ownership is unknown |
| accepted-input observation | `['conversation-initial-input', trackId, cardId]`, plus track and global prefixes |
| transcript / planner run | `['harness-items', cardId]` / `['planner-run', cardId]` |
| plugins / templates / plugin data | `['plugins']` / `['track-templates']` / `['plugin-data', pluginId]` (all windows) |
| Today resolve / model catalog | `['today-launchpad']` / `['model-catalog', scope]` |

`invalidation-baseline.test.ts` records actual operations through
`invalidationPlanFor → applyEventEffects` on every wire kind, with resolved and
unresolved ownership. Its snapshot was captured before this refactor. Prefix
matching, cancel-before-invalidate, transcript resets and removals are preserved.
The task events with only opaque task ids use the report **prefix**; ids are
never parsed. `task.git_delivery_settled` is different: its released payload
explicitly names a track, so its pre-existing scoped invalidation remains.

Resulting per-kind behavior (all unchanged by the address refactor):

| Event kind | Effect on cache |
| --- | --- |
| `area.updated` | write through an existing area, invalidate areas |
| `area.deleted` | retire that area's child queries, invalidate areas + track overlays + all task verdicts |
| `track.updated` | invalidate that area's track list + track detail + all task verdicts |
| `track.deleted` | invalidate area's track list + track overlays + all task verdicts; remove track detail, its verdicts and its accepted-input observations |
| `card.added` | invalidate track detail + conversation list + all task verdicts |
| `card.updated` | invalidate track detail + conversation list + card planner run + accepted-input observation |
| `card.deleted` | invalidate track detail + all task verdicts; remove that card's accepted-input observations |
| `worker_session.started` / `worker_session.status_changed` / `worker_session.superseded` | invalidate card overlays, resolved track detail, task verdicts, conversation list and accepted-input observations; unresolved ownership uses resource prefixes |
| `overlay.set` / `overlay.deleted` | invalidate overlays of that kind, plus track detail if ownership resolves |
| `track.report_edited` | invalidate all task verdicts and backlinks, the edited track detail and Today resolve |
| `plan.updated` with string `agent_message` | invalidate that track's verdicts; companion projection events without the message are no-ops |
| `terminal.deleted`, `codex.worker_requested`, `terminal.worker_requested` | invalidate task verdicts by named track or resolved card ownership, with prefix fallback |
| `task.dispatched`, `task.completed`, `task.failed`, `task.gate_result`, `task.regate_requested` | invalidate task verdicts by prefix only; opaque task ids are never parsed |
| `task.git_delivery_settled` | invalidate the explicitly named track's verdicts |
| `harness.item.added` | invalidate transcript + accepted-input observation; a persisted user message also refreshes track detail and conversation list |
| `harness.presentation.changed` | invalidate transcript + planner run + track detail + conversation list |
| `harness.phase.changed` | invalidate accepted-input observation + planner run + transcript + track detail + conversation list; `turn_completed` also refreshes the card's model catalog |
| `harness.transcript.cleared` / `harness.transcript.rewound` | reset transcript; invalidate accepted-input observation + transcript + planner run + track detail + conversation list |
| `harness.user_message.enqueued` / `harness.queue.changed` | invalidate accepted-input observation + transcript + planner run + track detail + conversation list; restored queue also resets transcript |
| `plugin.state` | invalidate plugins + track templates; transient-state polling remains |
| `plugin.data.changed` | invalidate that plugin's data prefix, reaching all cached calendar windows |
| `codex.hook`, `claude.hook`, other explicit no-op kinds | no cache operations; hooks write no task rows and have no workspace query |
| unknown / future kind | ignored, no throw |

Control frames: `replay-complete` → invalidate everything (`keys: null`) plus a
cursor write; `snapshot-required` → clear the cache, drop the cursor, and
reconnect.

### Effect ownership

- `persist-cursor` and `reconnect` are stream lifecycle; the bridge handles
  them, the adapter ignores them.
- `write-through` (`replace-existing-area`) explicitly converts the wire area
  with `toArea`, then replaces only a matching cached row. Missing rows are not
  inserted: the accompanying invalidation must refetch authoritative data.

## Resources without event queries

No query registers `track-files` or `tracks-range`; their former planned
addresses always fell out of the adapter. They are now removed from the plan.
If either resource gains a query, add its address to the core factory and its
policy to the plan together, with a production-entry-point contract test.

Cancel-first execution remains app policy: track detail, conversations, verdicts,
accepted-input observations, transcript, planner run, backlinks, plugins,
templates, plugin data and track overlays cancel before invalidation. Card
overlays retain ordinary invalidation. A replay boundary resets transcript
windows and cancels track overlays before invalidating everything. Confirmed
area deletion still calls `retireAreaQueries`; area updates still replace only
an existing row and preserve newer cached versions.

**Events are not sufficient for that panel, and the gap is on the kernel side.**
`scheduler::mark_running` stamps `worker_card_id` with no event at all —
`task.dispatched` fired before the spawn (column still NULL) and every
`worker_session.*` a worker adapter emits is emitted during the spawn, also before the
stamp. Between spawn and completion a terminal worker therefore emits nothing
that reaches this key, and an agent worker emits only hooks, which are excluded
above for cost. The panel closes that window with a bounded refresh timer on the
query itself, not with a new event or a re-included hook; see `hasLiveTaskRun`
in `core/domain/report.ts` for the accounting and `trackTaskVerdictsQueryOptions`
for the measured interval.
