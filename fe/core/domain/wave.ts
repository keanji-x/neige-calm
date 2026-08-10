// Wave: the unit of work the whole product is organised around. Wire decode,
// the lifecycle vocabulary, and the pure predicates that several surfaces must
// agree on (sidebar buckets, Today's counters, Today's agenda).

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const waveLifecycleSchema = z.enum([
  'draft', 'planning', 'dispatching', 'working',
  'blocked', 'reviewing', 'done', 'canceled', 'failed',
]);
export type WaveLifecycle = z.infer<typeof waveLifecycleSchema>;

/**
 * `lifecycle` / `cwd` / the `*_at` columns carry `#[serde(default)]` on the
 * kernel side for event-log replay, so they are absent from the OpenAPI
 * `required` set. The decoder supplies the documented DB defaults and the
 * decoded `Wave` keeps every field required.
 */
export const waveWireSchema = z.object({
  id: z.string(),
  cove_id: z.string(),
  title: z.string(),
  sort: z.number(),
  lifecycle: waveLifecycleSchema.default('draft'),
  cwd: z.string().default(''),
  archived_at: z.number().nullable().default(null),
  pinned_at: z.number().nullable().default(null),
  terminal_at: z.number().nullable().default(null),
  created_at: z.number(),
  updated_at: z.number(),
});
export type WaveWire = z.infer<typeof waveWireSchema>;

export type Wave = Readonly<{
  id: string;
  coveId: string;
  title: string;
  sort: number;
  lifecycle: WaveLifecycle;
  cwd: string;
  archivedAt: number | null;
  pinnedAt: number | null;
  terminalAt: number | null;
  createdAt: number;
  updatedAt: number;
}>;

export function toWave(wire: WaveWire): Wave {
  return {
    id: wire.id,
    coveId: wire.cove_id,
    title: wire.title,
    sort: wire.sort,
    lifecycle: wire.lifecycle,
    cwd: wire.cwd,
    archivedAt: wire.archived_at,
    pinnedAt: wire.pinned_at,
    terminalAt: wire.terminal_at,
    createdAt: wire.created_at,
    updatedAt: wire.updated_at,
  };
}

export function wavesInCoveOperation(coveId: string): ApiOperation<WaveWire[]> {
  return {
    method: 'GET',
    path: `/api/coves/${encodeURIComponent(coveId)}/waves`,
    responseSchema: z.array(waveWireSchema),
  };
}

/** The wave needs a human: blocked, in review, or failed. */
export function isWaitingForUser(lifecycle: WaveLifecycle): boolean {
  return lifecycle === 'blocked' || lifecycle === 'reviewing' || lifecycle === 'failed';
}

/** The wave has work in flight. `done` / `draft` / `canceled` are neither. */
export function isRunning(lifecycle: WaveLifecycle): boolean {
  return lifecycle === 'planning' || lifecycle === 'dispatching' || lifecycle === 'working';
}

export const UNTITLED_WAVE_LABEL = 'Untitled wave';

/** #409 — one display fallback for waves created without a title. */
export function waveDisplayTitle(title: string): string {
  return title.trim() || UNTITLED_WAVE_LABEL;
}

/** The canonical lifecycle phrase. Every surface reads it from here so the
 *  sidebar, the badge, and the agenda cannot drift into parallel tables. */
export function lifecycleLabel(lifecycle: WaveLifecycle): string {
  switch (lifecycle) {
    case 'draft': return 'Draft';
    case 'planning': return 'Planning';
    case 'dispatching': return 'Dispatching';
    case 'working': return 'Working';
    case 'blocked': return 'Blocked';
    case 'reviewing': return 'In review';
    case 'done': return 'Done';
    case 'canceled': return 'Canceled';
    case 'failed': return 'Failed';
  }
}

function startOfDay(day: Date): number {
  const start = new Date(day);
  start.setHours(0, 0, 0, 0);
  return start.getTime();
}

function endOfDay(day: Date): number {
  const end = new Date(day);
  end.setHours(23, 59, 59, 999);
  return end.getTime();
}

/**
 * #250 PR 5 — every wave whose `[createdAt, terminalAt ?? nowMs]` interval
 * overlaps the local day owning `day`.
 *
 * Endpoints are inclusive (`createdAt <= endOfDay AND end >= startOfDay`) so a
 * wave created at 23:59 still surfaces on that day even if its first card
 * lands a millisecond later. Sorted by `createdAt`, ties broken by id, so dot
 * ordering matches creation order (oldest leftmost — how the eye scans).
 */
export function activeWavesOn(waves: readonly Wave[], day: Date, nowMs: number): Wave[] {
  const dayStart = startOfDay(day);
  const dayEnd = endOfDay(day);
  const matched = waves.filter((wave) => {
    const end = wave.terminalAt ?? nowMs;
    return wave.createdAt <= dayEnd && end >= dayStart;
  });
  return matched.sort((left, right) => (left.createdAt !== right.createdAt
    ? left.createdAt - right.createdAt
    : left.id < right.id ? -1 : left.id > right.id ? 1 : 0));
}
