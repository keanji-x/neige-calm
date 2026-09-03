# `app/router`

Code-based TanStack Router. The tree is built by `createRouteTree` /
`createAppRouter`: nothing is constructed at module scope, and the transport and
QueryClient are injected so a test can drive a real tree.

## Routes

| path | state |
|---|---|
| `/` | Today, fully wired |
| `/area/$areaId/new` | creates a Track inside the selected Area |
| `/track/$trackId` | registered, renders `PendingRoute` (owner `features/track`) |
| `/settings` | registered, renders `PendingRoute` (owner `features/settings`) |

Area itself has no route: it is a disclosure group in the rail. Track and
Settings remain page destinations.

## Deliberate gaps

- **INV-APP-084** — the index loader primes **only** the areas list. The
  area → tracks fan-out stays lazy in the page (`useQueries` inside
  `useWorkspace`); awaiting it in the loader would let one slow area block the
  whole calendar behind the route commit.
- **INV-A11Y-061** — `useGo` is the single navigation exit and callers are
  buttons. Do not spread native links around; mixing forks Tab and activation
  semantics.
- No `basepath`. The legacy app serves under `/calm/`; this one is still a
  standalone dev surface.
