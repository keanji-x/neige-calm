// Track-lifecycle E2E helpers — issue #269 P1.
//
// The spec daemon does NOT run in the replay binary (the kernel boots
// with `DaemonClient::new_stub()` + `CodexClient::new_stub()`), so the
// spec-only lifecycle progressions (`planning → dispatching → working →
// reviewing → done`) can never happen organically in an a11y / replay
// run. `POST /dev/force-track-lifecycle` on the replay binary stamps a
// transition as `ActorId::Kernel` (which `track_lifecycle::actor_kind`
// classifies as `SpecAgent`) and writes through the same
// `write_with_events_typed` path the production `update_track` handler
// uses — same validator, same paired `TrackLifecycleChanged` +
// `TrackUpdated` events. The only thing this helper changes is **who**
// drives the edge, not whether the edge is legal.
//
// User-driven edges (Draft → Planning, Done → Planning reopen) don't
// need this helper — call `patchTrackLifecycle` (or PATCH /api/tracks/:id
// directly with no actor header) and the kernel attributes the write
// to `ActorId::User`.

import type { APIRequestContext, APIResponse } from '@playwright/test';

import { REPLAY_PORT } from './reset';

/**
 * The full set of `TrackLifecycle` variants. Mirrors the Rust enum in
 * `crates/calm-server/src/model.rs` and the lowercase serde tag in
 * `track_lifecycle.rs`'s `serde_round_trip_pinned_lowercase` test. Pinned
 * here as a TS string-literal union so e2e specs catch typos at
 * compile time rather than runtime.
 */
export type TrackLifecycle =
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
 * Force the track into `to` as if the spec agent had driven the edge.
 * Goes through `/dev/force-track-lifecycle` on the replay binary; throws
 * on non-2xx so a forbidden / illegal edge surfaces in the test that
 * triggered it rather than as a confusing later assertion failure.
 *
 * Use for spec-only edges (`planning → dispatching`, `dispatching →
 * working`, `reviewing → done`, etc.). For user-driven edges (kickoff,
 * cancel, reopen) use `patchTrackLifecycle` so the event log records the
 * write as User-driven, matching the production attribution.
 */
export async function forceTrackLifecycle(
  request: APIRequestContext,
  trackId: string,
  to: TrackLifecycle,
): Promise<{ track: TrackSnapshot; emitted_events: number }> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/force-track-lifecycle`;
  const response = await request.post(url, {
    data: { track_id: trackId, to },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `forceTrackLifecycle(${trackId}, ${to}): POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as { track: TrackSnapshot; emitted_events: number };
}

/**
 * Sibling of `forceTrackLifecycle` that returns the raw `APIResponse`
 * without throwing on 4xx. Used by the rejection-paths suite — the
 * tests there *expect* the validator to reject (e.g. `draft → done`
 * skip), so a throwing helper would defeat the point. Callers are
 * responsible for asserting the status / parsing the body.
 *
 * Same wire shape, same headers, same actor (`ActorId::Kernel`) as
 * the throwing variant — only the error handling differs.
 */
export async function forceTrackLifecycleRaw(
  request: APIRequestContext,
  trackId: string,
  to: TrackLifecycle,
): Promise<APIResponse> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/force-track-lifecycle`;
  return request.post(url, {
    data: { track_id: trackId, to },
    headers: { 'content-type': 'application/json' },
  });
}

/**
 * PATCH `/api/tracks/{id}` with `lifecycle: to`. No `X-Calm-Actor`
 * header → the kernel attributes the write to `ActorId::User`. Use for
 * kickoff (`draft → planning`), cancel, and reopen — the three user-
 * driven lifecycle paths.
 */
export async function patchTrackLifecycle(
  request: APIRequestContext,
  trackId: string,
  to: TrackLifecycle,
): Promise<TrackSnapshot> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${trackId}`;
  const response = await request.patch(url, {
    data: { lifecycle: to },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `patchTrackLifecycle(${trackId}, ${to}): PATCH ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as TrackSnapshot;
}

/**
 * Sibling of `patchTrackLifecycle` that returns the raw `APIResponse`
 * without throwing on 4xx, and accepts an optional `X-Calm-Actor`
 * header. The rejection-paths suite uses this for two flavors of
 * negative test:
 *   * default actor (User) attempting a spec-only edge → 403;
 *   * explicit `ai:codex` actor (classified as `Worker` by the
 *     lifecycle validator) attempting any edge → 403.
 * Callers are responsible for asserting the status / parsing the body.
 */
export async function patchTrackLifecycleRaw(
  request: APIRequestContext,
  trackId: string,
  to: TrackLifecycle,
  opts: { actorHeader?: string } = {},
): Promise<APIResponse> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${trackId}`;
  const headers: Record<string, string> = { 'content-type': 'application/json' };
  if (opts.actorHeader !== undefined) {
    headers['X-Calm-Actor'] = opts.actorHeader;
  }
  return request.patch(url, {
    data: { lifecycle: to },
    headers,
  });
}

/**
 * GET the current track detail and return the track row. Used by the
 * lifecycle suite to confirm `terminal_at` lands on entry to a terminal
 * state and clears on reopen.
 */
export async function getTrack(
  request: APIRequestContext,
  trackId: string,
): Promise<TrackSnapshot> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${trackId}`;
  const response = await request.get(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `getTrack(${trackId}): GET ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const detail = (await response.json()) as { track: TrackSnapshot };
  return detail.track;
}

/** Shape of the track row returned by `GET /api/tracks/{id}` (the detail
 *  envelope's `track` field) and by the force-lifecycle dev endpoint.
 *  Mirrors `crates/calm-server/src/model.rs::Track`; only the fields the
 *  lifecycle suite asserts on are pinned here. */
export interface TrackSnapshot {
  id: string;
  area_id: string;
  title: string;
  lifecycle: TrackLifecycle;
  terminal_at: number | null;
  created_at: number;
  updated_at: number;
}
