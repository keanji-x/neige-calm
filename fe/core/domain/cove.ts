// Cove: the workspace grouping a wave belongs to. Wire decode + the pure
// helpers every end shares. Platform-independent by construction — the
// transport is injected at the call site (core/api/client.ts).

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const coveKindSchema = z.enum(['user', 'system']);
export type CoveKind = z.infer<typeof coveKindSchema>;

/**
 * `kind` is absent from the OpenAPI `required` set because the kernel emits it
 * with `#[serde(default)]` for pre-#175 event-log replays. The default belongs
 * to the decoder, not to every reader: the decoded `Cove` keeps `kind`
 * required so no consumer has to re-derive it.
 */
export const coveWireSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  kind: coveKindSchema.default('user'),
  created_at: z.number(),
  updated_at: z.number(),
});
export type CoveWire = z.infer<typeof coveWireSchema>;

export type Cove = Readonly<{
  id: string;
  name: string;
  color: string;
  sort: number;
  kind: CoveKind;
  createdAt: number;
  updatedAt: number;
}>;

export function toCove(wire: CoveWire): Cove {
  return {
    id: wire.id,
    name: wire.name,
    color: wire.color,
    sort: wire.sort,
    kind: wire.kind,
    createdAt: wire.created_at,
    updatedAt: wire.updated_at,
  };
}

export function coveListOperation(): ApiOperation<CoveWire[]> {
  return { method: 'GET', path: '/api/coves', responseSchema: z.array(coveWireSchema) };
}

/**
 * E2E-INV-SHELL-003 — the system cove hosting the default Today terminal must
 * never reach a user-visible surface. `GET /api/coves` already filters it
 * server-side; this is the second layer of defence (#175) so a future
 * `?include_system=true` call site, or a replayed payload, cannot leak kernel
 * scaffolding into the sidebar or into Today's fan-out.
 */
export function visibleCoves(coves: readonly Cove[]): Cove[] {
  return coves.filter((cove) => cove.kind === 'user');
}

export function coveOf(coveId: string, coves: readonly Cove[]): Cove | undefined {
  return coves.find((cove) => cove.id === coveId);
}

export function sortedCoves(coves: readonly Cove[]): Cove[] {
  return [...coves].sort((left, right) => (left.sort !== right.sort
    ? left.sort - right.sort
    : left.id < right.id ? -1 : left.id > right.id ? 1 : 0));
}
