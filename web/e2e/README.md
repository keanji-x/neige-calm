# e2e tests

Playwright specs against the full running stack — kernel + sqlite + UI.

## Prereqs

1. Bring the stack up from the repo root:

   ```sh
   make dev
   ```

   This boots calm-server (on `:4040`) with the docker MockRepo.
   Issue #175 — the sidebar no longer ships with a seeded `Scratch`
   cove; the kernel's default Today terminal lives in a hidden system
   cove (filtered out of `GET /api/coves` by default). Each spec
   creates the user-visible coves and waves it needs as part of its
   setup, anchored on `localStorage['calm.todayCardId']` rather than
   on a seeded cove name.

2. One-time browser install (first run only):

   ```sh
   npx playwright install chromium
   ```

   We only install Chromium to keep the CI image small. Add other
   projects to `playwright.config.ts` if you need them.

## Running

```sh
cd web
npm run e2e        # headless
npm run e2e:ui     # Playwright UI mode — handy for new specs
```

If the stack isn't running you'll see `net::ERR_CONNECTION_REFUSED`
in the report. That's the expected failure mode — Playwright doesn't
boot the server (see the `webServer` comment in
`playwright.config.ts`).

## What's covered

- `golden-path.spec.ts` — sidebar renders, the Today page bootstraps
  a default terminal (system cove hidden — issue #175), and the user
  can mint a new cove via "+ New cove" and navigate into it. Catches
  a broken router, a broken `/api/coves` route, or a system-cove
  filter regression that would leak the kernel scaffolding back into
  the sidebar.

- `a11y-trace-smoke.spec.ts` — smoke test for the event-trace
  exposure plumbing (issue #56 slice 5). Runs under the separate
  `a11y` Playwright project (see "Projects" below).

- `a11y-keyboard.spec.ts` — keyboard-only e2e flows (issue #56
  slice 6). Tab / Enter / Space / Escape / F2 only — proves the
  app is drivable by an AI agent or any keyboard-only user. Pairs
  UI assertions with `window.__neigeEvents__` trace assertions
  where it matters. Runs under the `a11y` project.

- `a11y-axe.spec.ts` — axe-core scans (issue #56 slice 6) for
  Today / Cove / Wave / Settings + the AddPanel and Modal open
  states. Tagged to WCAG 2.1 A + AA + best-practice; any
  violation on a common page fails the spec. Runs under the
  `a11y` project.

- `a11y-spec-chat-{seed,live,input,interrupt}.spec.ts` — browser-level
  regression pins for the SpecCurrentRun / spec-chat UI (issue #682;
  anchored on the #676 dead-Stop-chip and #657 dead-typing-indicator
  incidents). The replay stub can't progress the harness FSM, so the
  specs drive phases through `POST /dev/force-spec-phase` (see
  `helpers/spec-chat.ts`): seed (`GET /spec/run` opens working UI on a
  mid-turn page load), live (`harness.phase.changed` updates without
  reload + snake_case wire-shape pin), input (`POST /spec/input` happy
  path, no phase churn), interrupt (Stop chip / ■ / Esc fire
  `POST /spec/interrupt`; probed stub outcome pinned). Run under the
  `a11y` project.

Add more specs as flows stabilize — keep them as narrowly scoped as
the golden path so a single broken seed doesn't take the whole
suite down.

## A11y scripts

The keyboard + axe suites come with their own npm scripts, all of
which run under the `a11y` Playwright project (requires `cargo`):

```sh
npm run a11y:e2e   # keyboard-only flows (a11y-keyboard.spec.ts)
npm run a11y:axe   # axe scans       (a11y-axe.spec.ts)
npm run a11y       # lint + keyboard + axe in sequence
```

`npm run a11y` is the local gate for a11y-touching PRs. CI does
not yet invoke any Playwright project — these are run locally.

## Projects

`playwright.config.ts` defines two test projects with different
backends:

- **`chromium`** (default): targets the developer `make dev` stack
  on `:4040`. Use for any test that exercises the real seeded
  MockRepo. Specs: `golden-path.spec.ts`, `wave-create.spec.ts`.
  Run with `npx playwright test --project=chromium`.

- **`a11y`**: targets the `cargo run --features fixtures --bin replay -- --serve` binary
  spawned by `_setup/replay-server.setup.ts`, preloaded with a
  curated event-trace fixture from
  `crates/calm-server/tests/fixtures/events/`. Each test starts
  from a hermetic, known-state server, and tests can read the
  event trace via `helpers/trace.ts` (`window.__neigeEvents__`).
  Run with `npx playwright test --project=a11y`. Requires `cargo`
  on PATH; default fixture override via `NEIGE_FIXTURE=<path>`.

The default `npx playwright test` (no `--project=`) runs both —
which means cargo must be available. CI does not currently invoke
Playwright; e2e specs are run locally only.
