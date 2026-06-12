// Spec-chat E2E helpers — issue #682 PR-2.
//
// The replay binary boots the shared codex app-server as a stub, so the
// `spec-harness-start` operation submitted by `POST /api/waves` fails at
// `validate` — the spec card exists but has no runtime row and no
// registered harness, and the harness FSM can never progress organically.
// `POST /dev/force-spec-phase` (issue #682 PR-1, see
// `crates/calm-server/src/replay.rs::force_spec_phase`) closes that gap:
// it stands a paused harness up on registry miss and forces the FSM state
// through the regular `persist_snapshot` path, so `GET /spec/run`, the
// `harness.phase.changed` WS event, and the DB snapshot agree by
// construction. These helpers mirror `helpers/lifecycle.ts`.
//
// Probed stub-runtime facts the spec-chat suite leans on (pinned against
// the live replay binary, 2026-06; re-probe before relying on more):
//   * forced phases are stable — the dev harness never issues codex RPCs,
//     so `turn_running` / `turn_completed` stay put until the next force;
//   * EXCEPT `resumed`, which decays to `idle` after ~5s with an extra
//     `harness.phase.changed` event — don't assert it stays;
//   * `POST /spec/input` is a pure MPSC enqueue (200, no phase churn);
//   * `POST /spec/interrupt` at `turn_running` answers 200
//     `{stopped: true}` and parks the harness at `issuing_interrupt`,
//     where a 30s watchdog will wedge it — interrupt tests must act,
//     assert, and let the next `dev/reset` clean up (never idle >30s).

import type { APIRequestContext } from '@playwright/test';

import { REPLAY_PORT } from './reset';

/**
 * Forceable `HarnessPhaseTag` wire values. Mirrors the snake_case serde
 * tags in `crates/calm-server/src/harness.rs`; `wedged` is deliberately
 * absent — the dev endpoint rejects it with 400 (a failed runtime row is
 * no longer projectable by `GET /spec/run`).
 */
export type SpecHarnessPhase =
  | 'pending_thread_start'
  | 'idle'
  | 'issuing_turn'
  | 'issuing_interrupt'
  | 'turn_running'
  | 'turn_completed'
  | 'resumed';

/** Response body of `POST /dev/force-spec-phase`. */
export interface ForceSpecPhaseResult {
  ok: boolean;
  card_id: string;
  runtime_id: string;
  old_phase: string;
  new_phase: string;
}

/**
 * Force the spec card's harness into `to` via the replay binary's dev
 * hook. Stands the harness up automatically when none is registered
 * (first call after wave creation / reset). Throws on non-2xx so an
 * unsupported tag or a non-spec card surfaces in the test that triggered
 * it rather than as a confusing later assertion failure.
 */
export async function forceSpecPhase(
  request: APIRequestContext,
  cardId: string,
  to: SpecHarnessPhase,
): Promise<ForceSpecPhaseResult> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/force-spec-phase`;
  const response = await request.post(url, {
    data: { card_id: cardId, to },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `forceSpecPhase(${cardId}, ${to}): POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as ForceSpecPhaseResult;
}

/**
 * Discover the spec card auto-created by `POST /api/waves`. The wave
 * detail (`GET /api/waves/{id}` → `{wave, cards, overlays}`) carries every
 * card row; the spec card is the `kind: "codex"` row whose payload has the
 * `spec_harness: true` marker (`routes/waves.rs::spec_harness_card_payload`)
 * — the same predicate `WaveReportPage.selectSpecCard` resolves against
 * the FE card slots. Throws when the wave has no spec card so a seeding
 * regression fails the test at setup rather than at a later locator.
 */
export async function getSpecCardId(
  request: APIRequestContext,
  waveId: string,
): Promise<string> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/waves/${encodeURIComponent(waveId)}`;
  const response = await request.get(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `getSpecCardId(${waveId}): GET ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const detail = (await response.json()) as {
    cards: { id: string; kind: string; payload: Record<string, unknown> }[];
  };
  const spec = detail.cards.find(
    (c) => c.kind === 'codex' && c.payload['spec_harness'] === true,
  );
  if (!spec) {
    throw new Error(
      `getSpecCardId(${waveId}): no spec codex card in wave detail (cards: ${detail.cards
        .map((c) => `${c.id}:${c.kind}`)
        .join(', ')})`,
    );
  }
  return spec.id;
}

/** Response body of `GET /api/cards/{id}/spec/run` — the FE's seed read. */
export interface SpecRunSnapshot {
  card_id: string;
  runtime_id: string | null;
  phase: string | null;
}

/**
 * Read the harness phase snapshot the FE seeds from on mount. A dormant
 * card (no forced harness yet) answers `{runtime_id: null, phase: null}`.
 */
export async function getSpecRun(
  request: APIRequestContext,
  cardId: string,
): Promise<SpecRunSnapshot> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/cards/${encodeURIComponent(cardId)}/spec/run`;
  const response = await request.get(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `getSpecRun(${cardId}): GET ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as SpecRunSnapshot;
}
