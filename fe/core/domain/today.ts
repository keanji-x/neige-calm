// The Today launchpad resolve (#1253 §5.1).
//
// Today's document is not a thing this frontend invents: it is the `wave-report`
// card of the kernel's single `purpose = 'launchpad'` wave, read through the
// ordinary wave detail and located by `readWaveReport` in `./report.ts`. What
// this module adds is the one fact the wave detail cannot answer — **has that
// report ever been written by anyone?**
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
// bootstrap — materializes a workspace and waits on a harness operation. 404
// is its normal "not there yet" answer, which is an empty state and not an
// error.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const todayLaunchpadSchema = z.object({
  wave_id: z.string(),
  /**
   * The server's answer to "has anyone written this report".
   *
   * It is an approximation of "has today's summary run", and the name says
   * which one it actually is. It compares the report's `summary` + `body`
   * against the canonical freshly-minted pair and ignores `doc_rev`/`blocks`,
   * so a CRDT-materialized placeholder still reads `false`; and it flips on
   * *any* writer, including a human editing the document by hand, and never
   * flips back.
   */
  report_has_noninitial_content: z.boolean(),
});

export type TodayLaunchpadWire = z.infer<typeof todayLaunchpadSchema>;

/** The Today page load's only new request. Read-only; 404 ⇒ nothing yet. */
export function todayLaunchpadOperation(): ApiOperation<TodayLaunchpadWire> {
  return { method: 'GET', path: '/api/today/launchpad', responseSchema: todayLaunchpadSchema };
}
