// Planner-chat E2E helpers — issue #682 PR-2.
//
// The replay binary boots the shared codex app-server as a stub, so the
// `planner-harness-start` operation submitted by `POST /api/tracks` fails at
// `validate` — the planner card exists but has no runtime row and no
// registered harness, and the harness FSM can never progress organically.
// `POST /dev/force-planner-phase` (issue #682 PR-1, see
// `crates/calm-server/src/replay.rs::force_planner_phase`) closes that gap:
// it stands a paused harness up on registry miss and forces the FSM state
// through the regular `persist_snapshot` path, so `GET /planner/run`, the
// `harness.phase.changed` WS event, and the DB snapshot agree by
// construction. These helpers mirror `helpers/lifecycle.ts`.
//
// Probed stub-runtime facts the planner-chat suite leans on (pinned against
// the live replay binary, 2026-06; re-probe before relying on more):
//   * forced phases are stable — the dev harness never issues codex RPCs,
//     so `turn_running` / `turn_completed` stay put until the next force;
//   * EXCEPT `resumed`, which decays to `idle` after ~5s with an extra
//     `harness.phase.changed` event — don't assert it stays;
//   * `POST /planner/input` is a pure MPSC enqueue (200, no phase churn);
//   * `POST /planner/interrupt` at `turn_running` answers 200
//     `{stopped: true}` and parks the harness at `issuing_interrupt`,
//     where a 30s watchdog will wedge it — interrupt tests must act,
//     assert, and let the next `dev/reset` clean up (never idle >30s).

import type { APIRequestContext, Page } from '@playwright/test';

import { REPLAY_PORT } from './reset';

/**
 * FE copy the planner-chat suite pins verbatim (lives in
 * `web/src/pages/PlannerConversation.tsx`). Centralized so a copy change
 * breaks exactly one constant instead of assertions scattered across
 * the input + interrupt specs.
 */
export const PLANNER_CHAT_COPY = {
  /** FE-local system note after a successful interrupt (#668). */
  turnStopped: 'Turn stopped',
  /** Author label on the FE-local echo entry of a queued user message. */
  queuedEcho: 'You · queued',
} as const;

/**
 * Forceable `HarnessPhaseTag` wire values. Mirrors the snake_case serde
 * tags of `HarnessPhaseTag` in `crates/calm-server/src/harness/snapshot.rs`;
 * `wedged` is deliberately absent — the dev endpoint rejects it with 400
 * (a failed runtime row is no longer projectable by `GET /planner/run`).
 */
export type PlannerHarnessPhase =
  | 'pending_thread_start'
  | 'idle'
  | 'issuing_turn'
  | 'issuing_interrupt'
  | 'turn_running'
  | 'turn_completed'
  | 'resumed';

/** Response body of `POST /dev/force-planner-phase`. */
export interface ForcePlannerPhaseResult {
  ok: boolean;
  card_id: string;
  runtime_id: string;
  old_phase: string;
  new_phase: string;
}

/**
 * Force the planner card's harness into `to` via the replay binary's dev
 * hook. Stands the harness up automatically when none is registered
 * (first call after track creation / reset). Throws on non-2xx so an
 * unsupported tag or a non-planner card surfaces in the test that triggered
 * it rather than as a confusing later assertion failure.
 */
export async function forcePlannerPhase(
  request: APIRequestContext,
  cardId: string,
  to: PlannerHarnessPhase,
): Promise<ForcePlannerPhaseResult> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/force-planner-phase`;
  const response = await request.post(url, {
    data: { card_id: cardId, to },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `forcePlannerPhase(${cardId}, ${to}): POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as ForcePlannerPhaseResult;
}

/**
 * Discover the planner card auto-created by `POST /api/tracks`. The track
 * detail (`GET /api/tracks/{id}` → `{track, cards, overlays}`) carries every
 * card row; the planner card is the `kind: "codex"` row whose payload has the
 * `planner_harness: true` marker (`routes/tracks.rs::planner_harness_card_payload`)
 * — the same predicate `TrackReportPage.selectPlannerCard` resolves against
 * the FE card slots. Throws when the track has no planner card so a seeding
 * regression fails the test at setup rather than at a later locator.
 */
export async function getPlannerCardId(
  request: APIRequestContext,
  trackId: string,
): Promise<string> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${encodeURIComponent(trackId)}`;
  const response = await request.get(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `getPlannerCardId(${trackId}): GET ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const detail = (await response.json()) as {
    cards: { id: string; kind: string; payload: Record<string, unknown> }[];
  };
  const planner = detail.cards.find(
    (c) => c.kind === 'codex' && c.payload['planner_harness'] === true,
  );
  if (!planner) {
    throw new Error(
      `getPlannerCardId(${trackId}): no planner codex card in track detail (cards: ${detail.cards
        .map((c) => `${c.id}:${c.kind}`)
        .join(', ')})`,
    );
  }
  return planner.id;
}

/** Response body of `GET /api/cards/{id}/planner/run` — the FE's seed read. */
export interface PlannerRunSnapshot {
  card_id: string;
  runtime_id: string | null;
  phase: string | null;
}

/**
 * Read the harness phase snapshot the FE seeds from on mount. A dormant
 * card (no forced harness yet) answers `{runtime_id: null, phase: null}`.
 */
export async function getPlannerRun(
  request: APIRequestContext,
  cardId: string,
): Promise<PlannerRunSnapshot> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/cards/${encodeURIComponent(cardId)}/planner/run`;
  const response = await request.get(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `getPlannerRun(${cardId}): GET ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as PlannerRunSnapshot;
}

/**
 * Intercept the page's `/api/events` WebSocket and drop every
 * server→client `harness.phase.changed` frame; everything else (both
 * directions, including the client's `{sub, since}` publishes and the
 * server's `_replay_complete` control frame) proxies through untouched.
 *
 * Why: the event stream subscribes with `since: 0`, so a phase forced
 * BEFORE navigation is replayed to the fresh page over WS anyway — and
 * the replayed frame carries the same wire value as the REST seed. A
 * seed-path test without this filter could stay green even if the
 * component never consumed `GET /planner/run`. With the frames dropped,
 * the seed read is provably the ONLY liveness source.
 *
 * Must be installed before `page.goto`. Seed-path tests only — live /
 * input / interrupt specs depend on real phase frames.
 */
export async function dropServerPhaseFrames(page: Page): Promise<void> {
  await page.routeWebSocket(/\/api\/events/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      server.send(message);
    });
    server.onMessage((message) => {
      if (typeof message === 'string') {
        try {
          const parsed = JSON.parse(message) as { ev?: unknown };
          if (parsed.ev === 'harness.phase.changed') return;
        } catch {
          // Non-JSON frame — pass through.
        }
      }
      ws.send(message);
    });
  });
}
