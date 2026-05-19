# e2e tests

Playwright specs against the full running stack — kernel + sqlite + UI.

## Prereqs

1. Bring the stack up from the repo root:

   ```sh
   make dev
   ```

   This boots calm-server (on `:4040`) with the docker MockRepo. The
   default seed includes a `Scratch` cove that the golden-path test
   anchors on.

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

- `golden-path.spec.ts` — sidebar renders, clicking the seeded
  `Scratch` cove navigates to `/calm/cove/$id`. Just enough to
  catch a broken router or a broken `/api/coves` route.

Add more specs as flows stabilize — keep them as narrowly scoped as
the golden path so a single broken seed doesn't take the whole
suite down.
