# `app/router`

Code-based TanStack Router. The tree is built by `createRouteTree` /
`createAppRouter`: nothing is constructed at module scope, and the transport and
QueryClient are injected so a test can drive a real tree.

## Routes

| path | state |
|---|---|
| `/` | Today, fully wired |
| `/cove/$coveId` | registered, renders `PendingRoute` (owner `features/cove`) |
| `/wave/$waveId` | registered, renders `PendingRoute` (owner `features/wave`) |
| `/settings` | registered, renders `PendingRoute` (owner `features/settings`) |

The three pending routes exist so navigation from the rail commits a real URL and
the active-row highlight works. Replace one by swapping its `component`.

## Deliberate gaps

- **INV-APP-084** — the index loader primes **only** the coves list. The
  cove → waves fan-out stays lazy in the page (`useQueries` inside
  `useWorkspace`); awaiting it in the loader would let one slow cove block the
  whole calendar behind the route commit.
- **INV-A11Y-061** — `useGo` is the single navigation exit and callers are
  buttons. Do not spread native links around; mixing forks Tab and activation
  semantics.
- No `basepath`. The legacy app serves under `/calm/`; this one is still a
  standalone dev surface.
