// The Today launchpad resolve (#1253 §5.1).
//
// Today's document is not a thing this frontend invents: it is the `track-report`
// card of the kernel's single `purpose = 'launchpad'` track, read through the
// ordinary track detail and located by `readTrackReport` in `./report.ts`. What
// this module adds is the one fact the track detail cannot answer — **does that
// report currently hold anything other than its freshly-minted skeleton?**
//
// That question has to be answered by the server, and the reason is the whole
// point of the module. `readTrackReport` returns a NON-null report for a
// freshly-minted, never-written report: the kernel's `initial()` body carries
// the maintenance-contract comment and the four default H1s, so it is a
// perfectly well-formed document that happens to say nothing. A frontend that
// decided "is there progress yet?" by looking at the report — null-checking it,
// measuring its length, or matching its text — renders four empty headings
// where the empty state belongs, and it would be mirror code for a body the
// kernel owns besides. The server computes the answer with its own canonical
// predicate (`TrackReportPayload::report_startup_read_required`) and sends it as
// `report_has_noninitial_content`. Nothing here parses the report body.
//
// The endpoint is a pure read. It deliberately does NOT bootstrap: the Today
// page load must not depend on codex being reachable, and `ensure` — the
// bootstrap — materializes a workspace and waits on a harness operation. Its
// normal "not there yet" answer is a 200 carrying `null`, which is an empty
// state and not an error.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import { trackConversationCardId, type Conversation } from './conversation.js';

export const todayLaunchpadSchema = z.object({
  track_id: z.string(),
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
 * **`null` is data, not a failure.** A fresh workspace has no launchpad track,
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

export const todayLaunchpadEnsureSchema = z.object({ track_id: z.string() });

export type TodayLaunchpadEnsureWire = z.infer<typeof todayLaunchpadEnsureSchema>;

/**
 * Materialise the Today launchpad after an explicit user action.
 *
 * This operation is deliberately separate from the page-load resolve above:
 * it starts the launchpad harness and may therefore wait on the agent service.
 * It sends no client-authored launchpad shape or report body.
 */
export function todayLaunchpadEnsureOperation(): ApiOperation<TodayLaunchpadEnsureWire> {
  return {
    method: 'POST',
    path: '/api/today/launchpad/ensure',
    responseSchema: todayLaunchpadEnsureSchema,
  };
}

export const todayReportResetSchema = z.object({
  /** The launchpad track whose report was restored. */
  track_id: z.string(),
  /**
   * What `GET /api/today/launchpad` will now report — always `false`.
   *
   * Returned by the server rather than assumed here so the caller can see the
   * reset land without a second round trip. It is still the *server's*
   * predicate, computed by the same byte comparison the resolve uses; nothing
   * on this side re-derives it.
   */
  report_has_noninitial_content: z.boolean(),
});

export type TodayReportResetWire = z.infer<typeof todayReportResetSchema>;

/** The server's fixed idempotency key for the launchpad's summary writer. */
export const TODAY_SUMMARY_CONVERSATION_KEY = 'today-summary';
export const TODAY_SUMMARY_CONVERSATION_TITLE = 'Today’s progress';

/**
 * Give the server-synthesised summary writer a reader-facing name. Its first
 * persisted user turn is an internal bootstrap instruction, so the ordinary
 * "first thing said" fallback would expose implementation text as the title.
 * A future explicit server title wins unchanged.
 */
export function nameTodaySummaryConversation(trackId: string, row: Conversation): Conversation {
  if (row.title !== null || row.id !== trackConversationCardId(trackId, TODAY_SUMMARY_CONVERSATION_KEY)) {
    return row;
  }
  return { ...row, title: TODAY_SUMMARY_CONVERSATION_TITLE };
}

/**
 * Put today's report back to its canonical empty state (#1343).
 *
 * **It sends no document, and there must never be a parameter for one.** The
 * empty-state predicate is a byte-for-byte comparison against the kernel's
 * `TrackReportPayload::initial()`, whose body is two `include_str!`-ed contract
 * fragments plus four empty H1s. A client that posted its own copy of those
 * bytes to `POST /api/tracks/{id}/report` would be mirror code for kernel-owned
 * text, and one byte out fails *silently*: a 200, a rewritten report, and an
 * empty state that never appears. So the kernel writes its own canonical
 * document and nothing about it crosses the wire.
 *
 * It is destructive — the day's report is discarded — and it touches the report
 * only: no conversation is created, reset or deleted.
 */
export function todayReportResetOperation(): ApiOperation<TodayReportResetWire> {
  return {
    method: 'POST',
    path: '/api/today/launchpad/report/reset',
    responseSchema: todayReportResetSchema,
  };
}
