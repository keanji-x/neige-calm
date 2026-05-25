// Smoke test for the event-trace exposure plumbing (issue #56 slice 5).
//
// Prerequisite: Playwright's `a11y` project must be active (so the
// `_setup/replay-server.ts` globalSetup spawned the replay binary with
// the wave-grid-layout fixture preloaded — see `playwright.config.ts`).
// Running this spec from the default `chromium` project will fail with a
// connection-refused error because that project still targets the
// developer `make dev` stack on :4041.
//
// What we're proving end-to-end:
//   1. The dev build + `?trace=1` URL param installs `window.__neigeEvents__`.
//   2. The shared WS connection drains the replay server's pre-seeded
//      event log on first connect.
//   3. The bridge mirrors those events into the ring buffer in arrival
//      order, with the envelope `_id` / `eventVersion` stamped on each.
//   4. The fixture's event sequence shows up verbatim — i.e. tests can
//      reason about the trace shape, not just the resulting UI state.
//
// Slice 6 will write the actual a11y assertions on top of this; we keep
// the smoke test minimal so it documents the helper API without
// duplicating the assertions Slice 6 will own.

import { test, expect } from '@playwright/test';
import { resetReplayServer } from './helpers/reset';
import { getEventTrace, waitForEvent } from './helpers/trace';

test.beforeEach(async ({ request }) => {
  // Hermetic per-test state — see `helpers/reset.ts` for the rationale.
  // The smoke assertion below pins the exact fixture event sequence, so
  // any accumulated mutations from an earlier spec would break it.
  await resetReplayServer(request);
});

// Sequence the wave-grid-layout-trace fixture seeds, in order. Pinned
// here so a fixture edit causes a noisy failure — the smoke test's whole
// purpose is to anchor the contract between the fixture and the bridge.
const EXPECTED_FIXTURE_KINDS = [
  'cove.updated',
  'wave.updated',
  'card.added',
  'card.added',
  'card.added',
  'overlay.set',
  'overlay.set',
];

test('event trace ring buffer populates with the fixture sequence', async ({ page }) => {
  // baseURL is set by the a11y project in playwright.config.ts; we just
  // tag `?trace=1` on the initial nav. EventBridge reads the URL once on
  // mount, so opening any page with the param is enough.
  await page.goto('/?trace=1');

  // First, wait for the buffer to come into existence. The bridge writes
  // it lazily on first frame, so this also implies "WS connected and
  // delivered at least one event".
  await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));

  // The fixture ends with an `overlay.set` — wait for that specifically
  // so we know the entire seed sequence has drained. The replay binary
  // streams the log on connect, then sends `_replay_complete` (which the
  // bridge handles but isn't put in the trace).
  const lastSeeded = await waitForEvent(page, 'overlay.set');
  expect(lastSeeded.id).toBeGreaterThan(0);
  expect(lastSeeded.eventVersion).toBeGreaterThan(0);

  const trace = await getEventTrace(page);
  expect(trace.length).toBeGreaterThanOrEqual(EXPECTED_FIXTURE_KINDS.length);
  // Compare the first N kinds against the fixture; trailing entries (if
  // any) would be the bridge's own runtime echoes during page boot — we
  // don't pin them here.
  expect(trace.slice(0, EXPECTED_FIXTURE_KINDS.length).map((e) => e.ev)).toEqual(
    EXPECTED_FIXTURE_KINDS,
  );
});
