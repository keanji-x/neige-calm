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

`npm run a11y` is the gate for a11y-touching PRs — CI runs it in
the `web (build + test + a11y)` job (`.github/workflows/ci.yml`),
so anything not wired into it does not run in CI. The scripts
(all run under a Playwright project, so they require `cargo`):

```sh
npm run a11y:e2e       # the WHOLE a11y project (all a11y-*.spec.ts)
npm run a11y:color     # the color-anchor project (color-system-anchor)
npm run a11y:axe       # axe scans only   (a11y-axe.spec.ts)
npm run a11y:spec-chat # spec-chat pins   (a11y-spec-chat-*.spec.ts)
npm run a11y           # lint + a11y:e2e + a11y:color (the CI gate)
```

Note (#690): `a11y:e2e` runs the entire `a11y` project via
`--project=a11y` — its testMatch is `**/a11y-*.spec.ts`, so EVERY
`a11y-*.spec.ts` is in the gate automatically, including keyboard,
axe, spec-chat, the wave/cove lifecycle + ops + rename/delete specs,
cwd-resolve, deep-link-after-reset, sidebar-wave-delete, trace-smoke,
and the #177 theme-toggle spec (which self-skips when `codex` is
absent). No per-file registration needed — add an `a11y-*.spec.ts`
and it's gated. `a11y:axe` / `a11y:spec-chat` are kept as standalone
dev conveniences for running just those subsets; they are NOT in the
`a11y` chain (the whole-project `a11y:e2e` already covers them, so
re-listing them would double-run those files). `a11y:color` runs the
separate `color-anchor` project (it lives in its own project because
it boots a custom `color-anchor.html` harness page rather than the
SPA).

## Projects

`playwright.config.ts` defines four projects (two test, two
setup/teardown helpers):

- **`chromium`** (default): targets the developer `make dev` stack
  on `:4040`. Use for any test that exercises the real seeded
  MockRepo. Specs: `golden-path.spec.ts`, `wave-create.spec.ts`.
  Run with `npx playwright test --project=chromium`.

- **`a11y`**: testMatch `**/a11y-*.spec.ts`. Targets the
  `cargo run --features fixtures --bin replay -- --serve` binary
  spawned by `_setup/replay-server.setup.ts`, preloaded with a
  curated event-trace fixture from
  `crates/calm-server/tests/fixtures/events/`. Each test starts
  from a hermetic, known-state server, and tests can read the
  event trace via `helpers/trace.ts` (`window.__neigeEvents__`).
  Run with `npx playwright test --project=a11y`. Requires `cargo`
  on PATH; default fixture override via `NEIGE_FIXTURE=<path>`.

- **`color-anchor`**: testMatch `**/color-system-anchor.spec.ts`.
  Same replay-binary backend as `a11y` (it now declares
  `dependencies: ['replay-setup']` so a standalone run boots the
  binary instead of connection-refusing — #690), but drives a
  dedicated `color-anchor.html` harness page to snapshot computed
  form-control colors across light/dark. Asserts color-scheme
  correctness + dark-bg sanity (it regenerates the informational
  `__snapshots__/color-anchor-baseline.md` on each run; it does NOT
  diff against the committed baseline, so it isn't environment-
  fragile). Run with `npx playwright test --project=color-anchor`.

The default `npx playwright test` (no `--project=`) runs everything —
which means cargo must be available. CI's a11y gate runs the `a11y`
and `color-anchor` projects via `npm run a11y` (see "A11y scripts"
above); the `chromium` project needs the `make dev` stack and stays
local.

### What the a11y gate covers (#690)

Before #690 the gate ran an explicit 6-file list and silently
skipped 9 specs that the `a11y` glob already owned. The gate now
runs the whole project, so these previously-ungated specs are live
in CI:

- `a11y-wave-lifecycle.spec.ts` / `a11y-wave-lifecycle-rejections.spec.ts`
  — wave lifecycle state machine + rejection edges.
- `a11y-wave-cove-ops.spec.ts` — wave/cove rename + delete (+ cascade)
  flows.
- `a11y-cwd-resolve.spec.ts` — cove-folder cwd resolution.
- `a11y-deep-link-after-reset.spec.ts` — #290 deep-link-after-reset
  race.
- `a11y-sidebar-wave-delete.spec.ts` — sidebar wave-delete affordance.
- `a11y-trace-smoke.spec.ts` — event-trace ring-buffer smoke.
- `a11y-177-theme-toggle-no-remount.spec.ts` — #177 XtermView remount
  guard (self-skips when `codex` is absent, e.g. on CI).
- `color-system-anchor.spec.ts` — via the `color-anchor` project.
