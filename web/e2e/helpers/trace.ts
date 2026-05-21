// Playwright helpers for inspecting the event-trace ring buffer that
// `EventBridge` installs on `window.__neigeEvents__` under dev + `?trace=1`.
//
// Slice 5 of issue #56 plumbs each WS frame into a 200-entry ring buffer
// (shape: `{id, eventVersion, ev, data, ts}`). These helpers are the
// browser-side counterpart — `page.evaluate` calls that read or clear the
// buffer, plus a couple of convenience assertions for keeping spec files
// concise.
//
// The TraceEvent type intentionally mirrors the bridge's exported shape
// (see `web/src/app/eventBridge.tsx`). Re-declared here rather than
// imported because Playwright specs sit outside the Vite TS project and
// pulling the source type would drag in a parallel tsconfig setup.

import { expect, type Page } from '@playwright/test';

export interface TraceEvent {
  id: number;
  eventVersion: number;
  ev: string;
  data: unknown;
  ts: number;
}

// Browser-side globals installed by `EventBridge` under dev + `?trace=1`.
// Mirrored here (not imported from src) because the e2e tree isn't part
// of the app's TS project; ambient declarations let `page.evaluate`
// callbacks be type-checked without a parallel tsconfig.
declare global {
  interface Window {
    __neigeEvents__?: TraceEvent[];
    __neigeClearEvents__?: () => void;
  }
}

/** Snapshot the current contents of `window.__neigeEvents__`. Returns an
 *  empty array if the buffer hasn't been initialized yet (e.g. the page
 *  hasn't loaded with `?trace=1`) — callers asserting "events arrived"
 *  should pair this with `waitForEvent` rather than relying on timing. */
export async function getEventTrace(page: Page): Promise<TraceEvent[]> {
  return page.evaluate(() => {
    return (window.__neigeEvents__ ?? []).slice() as TraceEvent[];
  });
}

/** Empty the ring buffer in place. Useful between scenarios in the same
 *  page session — replaces the contents without reassigning the global so
 *  any cached references in component code stay valid. */
export async function clearEventTrace(page: Page): Promise<void> {
  await page.evaluate(() => {
    window.__neigeClearEvents__?.();
  });
}

/** Assert the event kinds in the buffer match `expected` exactly, in order.
 *  Use when you want to pin "this exact sequence produced this UI state";
 *  for "the buffer eventually contained X" use `waitForEvent` instead. */
export async function assertEventKinds(page: Page, expected: string[]): Promise<void> {
  const actual = (await getEventTrace(page)).map((e) => e.ev);
  expect(actual, 'event trace kinds').toEqual(expected);
}

/** Poll the trace until an event with `ev === kind` shows up, then return
 *  it. Throws via the playwright timeout if it never appears.
 *
 *  Uses `page.waitForFunction` so the wait happens browser-side (sub-ms
 *  granularity, integrates with the page's animation frame). The default
 *  `timeoutMs` matches Playwright's `expect.toHaveText` default of 5s —
 *  long enough for a fresh WS connect + initial replay window to land. */
export async function waitForEvent(
  page: Page,
  kind: string,
  timeoutMs = 5000,
): Promise<TraceEvent> {
  const handle = await page.waitForFunction(
    (k) => {
      const buf = window.__neigeEvents__;
      if (!buf) return null;
      return buf.find((e) => e.ev === k) ?? null;
    },
    kind,
    { timeout: timeoutMs },
  );
  const value = (await handle.jsonValue()) as TraceEvent | null;
  if (!value) {
    throw new Error(`waitForEvent(${kind}): predicate resolved without a match`);
  }
  return value;
}
