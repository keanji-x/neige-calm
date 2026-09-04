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

import type { ApiFailure, ApiOperation } from '../api/types.js';
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

export const todaySummarySchema = z.object({
  /** The launchpad track, whose report the agent has been asked to rewrite. */
  track_id: z.string(),
  /**
   * The summary conversation's card — the same card for the launchpad's whole
   * lifetime, and openable in Today's Conversations module. It is derived from
   * a bare constant key server-side, so it does not move when the workspace
   * does (INV-TODAYDOC-011).
   */
  card_id: z.string(),
});

export type TodaySummaryWire = z.infer<typeof todaySummarySchema>;

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
 * Ask the server to write today's progress into Today's document (#1253 D5).
 *
 * **It sends no prompt, and there is no parameter for one.** The endpoint
 * synthesises the whole message server-side, from an activity projection this
 * frontend has no read for. That is not an omission to be filled in later: the
 * design deleted the layer that would have let a client — or an agent — ask for
 * activity, and a prompt parameter here would be the same hole with a nicer
 * name.
 *
 * A 200 means the request has been enqueued, not that the report has changed.
 * The agent's write arrives later as a `track.report_edited` event, and that is
 * what refreshes the page — see `core/events/invalidation-plan`, where that
 * kind carries `['today-launchpad']` and `['track', id]` precisely so this
 * button visibly does something.
 */
export function todaySummaryOperation(): ApiOperation<TodaySummaryWire> {
  return {
    method: 'POST',
    path: '/api/today/summary',
    responseSchema: todaySummarySchema,
  };
}

/**
 * The copy for the one refusal that is not a malfunction.
 *
 * The server counts four permanent event kinds over today's window and refuses
 * when they are all zero, creating no conversation and sending no message
 * (INV-TODAYDOC-007). That is a fact about the day, so it reads as one rather
 * than as an error: nothing is broken and there is nothing to retry.
 */
export const NOTHING_TO_SUMMARISE = 'Nothing has happened in this workspace today yet.';

export type TodaySummaryFailure = Readonly<{
  /** `'no-activity'` is data wearing a 409; the other two are failures. */
  kind: 'no-activity' | 'unavailable' | 'error';
  message: string;
}>;

/**
 * Classify a rejected summary trigger.
 *
 * The `code` is matched, not the status and not the message text. All three
 * kinds of 409 this endpoint can answer share a status — `conflict` from the
 * underlying create, `planner_harness_dormant` from a send that could not be
 * recovered, and this one — and only the code tells them apart. Matching the
 * sentence instead would be mirror code for a string the server owns.
 */
export function todaySummaryFailure(failure: ApiFailure): TodaySummaryFailure {
  if (failure.kind === 'transport' || failure.kind === 'decode') {
    return { kind: 'error', message: failure.message };
  }
  if (failure.code === 'today_summary_no_activity') {
    return { kind: 'no-activity', message: NOTHING_TO_SUMMARISE };
  }
  /* The agent service is down, which is not "something went wrong": the
     request was understood and can simply be made again. */
  if (failure.status === 503) {
    return { kind: 'unavailable', message: `The agent service is not available: ${failure.message}` };
  }
  return { kind: 'error', message: failure.message };
}

/**
 * `POST /api/today/launchpad/ensure` — materialise the launchpad track.
 *
 * **Not on any page-load path** (INV-TODAYDOC-001). It creates a workspace and
 * then waits on a harness start, so a route that rendered it would fail
 * hard whenever codex is down. Its one caller is the Today trigger, on a press,
 * and the reason that is not the same thing is written out where it is called
 * (`app/providers/queries.ts`).
 *
 * The response is `TodayLaunchpad`, a **different** shape from this module's
 * `todayLaunchpadSchema`: it carries the launchpad's two card ids and no
 * `report_has_noninitial_content`. Only `track_id` is read here — the trigger
 * needs to know the launchpad exists, and the page re-reads its actual state
 * through the resolve — and the rest is left unparsed rather than mirrored.
 */
export const todayLaunchpadEnsureSchema = z.object({ track_id: z.string() });

export type TodayLaunchpadEnsureWire = z.infer<typeof todayLaunchpadEnsureSchema>;

export function todayLaunchpadEnsureOperation(): ApiOperation<TodayLaunchpadEnsureWire> {
  return {
    method: 'POST',
    path: '/api/today/launchpad/ensure',
    responseSchema: todayLaunchpadEnsureSchema,
  };
}

/**
 * Classify a rejected `ensure`.
 *
 * Its own function rather than a `step` parameter on `todaySummaryFailure`,
 * because the two endpoints answer different things and the difference is the
 * whole point of the wording: `today_summary_no_activity` cannot come from here
 * (`ensure` never looks at the day), and a 503 here means the harness would not
 * start — the launchpad itself may well have been created, which is why the
 * page refetches the resolve even after this failure.
 *
 * There is no `'no-activity'` branch and there must not be one: an empty day is
 * a fact `POST /api/today/summary` alone can establish.
 */
export function todayLaunchpadEnsureFailure(failure: ApiFailure): TodaySummaryFailure {
  if (failure.kind === 'transport' || failure.kind === 'decode') {
    return { kind: 'error', message: failure.message };
  }
  if (failure.status === 503) {
    return {
      kind: 'unavailable',
      message: `Today’s workspace could not be started: ${failure.message}`,
    };
  }
  return { kind: 'error', message: `Today’s workspace could not be prepared: ${failure.message}` };
}
