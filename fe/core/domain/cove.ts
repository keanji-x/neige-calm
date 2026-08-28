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

export type NewCoveBody = Readonly<{ name: string; color: string; sort?: number }>;
export type CovePatchBody = Readonly<{ name?: string; color?: string; sort?: number }>;

export function createCoveOperation(body: NewCoveBody): ApiOperation<CoveWire> {
  return { method: 'POST', path: '/api/coves', body, responseSchema: coveWireSchema };
}

export function updateCoveOperation(coveId: string, body: CovePatchBody): ApiOperation<CoveWire> {
  return { method: 'PATCH', path: `/api/coves/${encodeURIComponent(coveId)}`, body, responseSchema: coveWireSchema };
}

export function deleteCoveOperation(coveId: string): ApiOperation<undefined> {
  return { method: 'DELETE', path: `/api/coves/${encodeURIComponent(coveId)}`, responseSchema: z.undefined() };
}

/**
 * A folder a cove has claimed. The kernel's `cove_folders` mapping, decoded.
 *
 * `id` is an autoincrement integer here rather than the usual uuid-shaped TEXT
 * because the row never enters the sync engine's event log — that is the
 * kernel's reason (see `CoveFolder` in `core/api/generated/wire.ts`), repeated
 * here only so a reader does not "fix" the type.
 *
 * `repo_identity` / `repo_identity_probed_at` are `null` until the kernel has
 * probed the folder's Git origin, so they stay nullable through the decode
 * rather than being defaulted into a lie.
 */
export const coveFolderWireSchema = z.object({
  id: z.number(),
  cove_id: z.string(),
  path: z.string(),
  repo_identity: z.string().nullable().default(null),
  repo_identity_probed_at: z.number().nullable().default(null),
  created_at: z.number(),
});
export type CoveFolderWire = z.infer<typeof coveFolderWireSchema>;

export type CoveFolder = Readonly<{
  id: number;
  coveId: string;
  path: string;
  repoIdentity: string | null;
  repoIdentityProbedAt: number | null;
  createdAt: number;
}>;

export function toCoveFolder(wire: CoveFolderWire): CoveFolder {
  return {
    id: wire.id,
    coveId: wire.cove_id,
    path: wire.path,
    repoIdentity: wire.repo_identity,
    repoIdentityProbedAt: wire.repo_identity_probed_at,
    createdAt: wire.created_at,
  };
}

export function coveFoldersOperation(coveId: string): ApiOperation<CoveFolderWire[]> {
  return {
    method: 'GET',
    path: `/api/coves/${encodeURIComponent(coveId)}/folders`,
    responseSchema: z.array(coveFolderWireSchema),
  };
}

/**
 * `path` ascending, ties broken by `id`. The kernel returns insertion order,
 * which is neither stable across a re-claim nor a useful display order.
 */
export function sortedCoveFolders(folders: readonly CoveFolder[]): CoveFolder[] {
  return [...folders].sort((left, right) => (left.path !== right.path
    ? (left.path < right.path ? -1 : 1)
    : left.id - right.id));
}

/** The eight identity slots. A cove's colour is a slot, never a free hex (§6.2). */
export const COVE_SLOT_COUNT = 8;

/**
 * §6.2 — a cove's identity dot is a stable hash of its id, mod 8, not the
 * kernel's `color` field. Two consequences the design leans on: the same cove
 * is the same colour on every surface and across reloads, and the palette stays
 * inside the token set, so the eight hues can be re-tuned (or cut to six) in
 * `tokens.css` without touching a component.
 *
 * It lives in core rather than beside COVE_PALETTE because three separate
 * surfaces need it and `features/**` may not import a sibling feature domain —
 * and because "which slot is this cove" is domain logic, not a palette value.
 */
export function coveSlotVar(coveId: string): string {
  let hash = 0;
  for (let index = 0; index < coveId.length; index += 1) {
    hash = (hash * 31 + coveId.charCodeAt(index)) | 0;
  }
  return `--cove-${(Math.abs(hash) % COVE_SLOT_COUNT) + 1}`;
}
