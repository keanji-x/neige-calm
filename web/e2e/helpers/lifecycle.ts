// Wave-lifecycle E2E helpers — issue #269 P1.
//
// The spec daemon does NOT run in the replay binary (the kernel boots
// with `DaemonClient::new_stub()` + `CodexClient::new_stub()`), so the
// spec-only lifecycle progressions (`planning → dispatching → working →
// reviewing → done`) can never happen organically in an a11y / replay
// run. `POST /dev/force-wave-lifecycle` on the replay binary stamps a
// transition as `ActorId::Kernel` (which `wave_lifecycle::actor_kind`
// classifies as `SpecAgent`) and writes through the same
// `write_with_events_typed` path the production `update_wave` handler
// uses — same validator, same paired `WaveLifecycleChanged` +
// `WaveUpdated` events. The only thing this helper changes is **who**
// drives the edge, not whether the edge is legal.
//
// User-driven edges (Draft → Planning, Done → Planning reopen) don't
// need this helper — call `patchWaveLifecycle` (or PATCH /api/waves/:id
// directly with no actor header) and the kernel attributes the write
// to `ActorId::User`.

import type { APIRequestContext } from '@playwright/test';

import { REPLAY_PORT } from './reset';

/**
 * The full set of `WaveLifecycle` variants. Mirrors the Rust enum in
 * `crates/calm-server/src/model.rs` and the lowercase serde tag in
 * `wave_lifecycle.rs`'s `serde_round_trip_pinned_lowercase` test. Pinned
 * here as a TS string-literal union so e2e specs catch typos at
 * compile time rather than runtime.
 */
export type WaveLifecycle =
  | 'draft'
  | 'planning'
  | 'dispatching'
  | 'working'
  | 'blocked'
  | 'reviewing'
  | 'done'
  | 'canceled'
  | 'failed';

/**
 * Force the wave into `to` as if the spec agent had driven the edge.
 * Goes through `/dev/force-wave-lifecycle` on the replay binary; throws
 * on non-2xx so a forbidden / illegal edge surfaces in the test that
 * triggered it rather than as a confusing later assertion failure.
 *
 * Use for spec-only edges (`planning → dispatching`, `dispatching →
 * working`, `reviewing → done`, etc.). For user-driven edges (kickoff,
 * cancel, reopen) use `patchWaveLifecycle` so the event log records the
 * write as User-driven, matching the production attribution.
 */
export async function forceWaveLifecycle(
  request: APIRequestContext,
  waveId: string,
  to: WaveLifecycle,
): Promise<{ wave: WaveSnapshot; emitted_events: number }> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/force-wave-lifecycle`;
  const response = await request.post(url, {
    data: { wave_id: waveId, to },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `forceWaveLifecycle(${waveId}, ${to}): POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as { wave: WaveSnapshot; emitted_events: number };
}

/**
 * PATCH `/api/waves/{id}` with `lifecycle: to`. No `X-Calm-Actor`
 * header → the kernel attributes the write to `ActorId::User`. Use for
 * kickoff (`draft → planning`), cancel, and reopen — the three user-
 * driven lifecycle paths.
 */
export async function patchWaveLifecycle(
  request: APIRequestContext,
  waveId: string,
  to: WaveLifecycle,
): Promise<WaveSnapshot> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/waves/${waveId}`;
  const response = await request.patch(url, {
    data: { lifecycle: to },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `patchWaveLifecycle(${waveId}, ${to}): PATCH ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as WaveSnapshot;
}

/**
 * GET the current wave detail and return the wave row. Used by the
 * lifecycle suite to confirm `terminal_at` lands on entry to a terminal
 * state and clears on reopen.
 */
export async function getWave(
  request: APIRequestContext,
  waveId: string,
): Promise<WaveSnapshot> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/waves/${waveId}`;
  const response = await request.get(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `getWave(${waveId}): GET ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const detail = (await response.json()) as { wave: WaveSnapshot };
  return detail.wave;
}

/** Shape of the wave row returned by `GET /api/waves/{id}` (the detail
 *  envelope's `wave` field) and by the force-lifecycle dev endpoint.
 *  Mirrors `crates/calm-server/src/model.rs::Wave`; only the fields the
 *  lifecycle suite asserts on are pinned here. */
export interface WaveSnapshot {
  id: string;
  cove_id: string;
  title: string;
  lifecycle: WaveLifecycle;
  terminal_at: number | null;
  created_at: number;
  updated_at: number;
}
