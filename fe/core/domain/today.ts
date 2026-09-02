// The Today launchpad resolve (#1253 §5.1).
//
// Today's document is not a thing this frontend invents: it is the `wave-report`
// card of the kernel's single `purpose = 'launchpad'` wave, read through the
// ordinary wave detail and located by `readWaveReport` in `./report.ts`. What
// this module adds is the one fact the wave detail cannot answer — **does that
// report currently hold anything other than its freshly-minted skeleton?**
//
// That question has to be answered by the server, and the reason is the whole
// point of the module. `readWaveReport` returns a NON-null report for a
// freshly-minted, never-written report: the kernel's `initial()` body carries
// the maintenance-contract comment and the four default H1s, so it is a
// perfectly well-formed document that happens to say nothing. A frontend that
// decided "is there progress yet?" by looking at the report — null-checking it,
// measuring its length, or matching its text — renders four empty headings
// where the empty state belongs, and it would be mirror code for a body the
// kernel owns besides. The server computes the answer with its own canonical
// predicate (`WaveReportPayload::report_startup_read_required`) and sends it as
// `report_has_noninitial_content`. Nothing here parses the report body.
//
// The endpoint is a pure read. It deliberately does NOT bootstrap: the Today
// page load must not depend on codex being reachable, and `ensure` — the
// bootstrap — materializes a workspace and waits on a harness operation. Its
// normal "not there yet" answer is a 200 carrying `null`, which is an empty
// state and not an error.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const todayLaunchpadSchema = z.object({
  wave_id: z.string(),
  /**
   * Whether the report's CURRENT content differs from a freshly-minted one.
   *
   * The name is the contract, and it is not "has ever been written": the
   * server compares `summary` + `body` against the canonical freshly-minted
   * pair and consults no history, so text restored byte-for-byte to that pair
   * reads `false` again whatever happened in between. `doc_rev` and `blocks`
   * are ignored, so a CRDT-materialized placeholder also reads `false`.
   *
   * It is therefore an approximation of "has today's summary run" in both
   * directions: any writer flips it, including a human editing by hand, and a
   * revert un-flips it.
   */
  report_has_noninitial_content: z.boolean(),
});

export type TodayLaunchpadWire = z.infer<typeof todayLaunchpadSchema>;

/**
 * The Today page load's only new request. Read-only; `null` ⇒ nothing yet.
 *
 * **`null` is data, not a failure.** A fresh workspace has no launchpad wave,
 * which is the ordinary state of this route, so the server says so with a 200
 * and a null body rather than a 404. It used to be a 404 and the frontend
 * translated it in `queries.ts`; that made every session on a fresh workspace
 * log a browser console error for a state the design calls normal, and broke
 * two Playwright specs that assert zero console errors. The translation is gone
 * along with the status code — no layer here treats any status specially now.
 *
 * `.nullable()` is applied here rather than hoisted to a module constant:
 * `architecture/no-module-runtime-state` forbids the resulting object at module
 * scope, and a schema built per call costs nothing on a once-per-page read.
 */
export function todayLaunchpadOperation(): ApiOperation<TodayLaunchpadWire | null> {
  return {
    method: 'GET',
    path: '/api/today/launchpad',
    responseSchema: todayLaunchpadSchema.nullable(),
  };
}
