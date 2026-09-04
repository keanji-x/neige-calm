// Area: the workspace grouping a track belongs to. Wire decode + the pure
// helpers every end shares. Platform-independent by construction — the
// transport is injected at the call site (core/api/client.ts).

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const areaKindSchema = z.enum(['user', 'system']);
export type AreaKind = z.infer<typeof areaKindSchema>;

/**
 * `kind` is absent from the OpenAPI `required` set because the kernel emits it
 * with `#[serde(default)]` for pre-#175 event-log replays. The default belongs
 * to the decoder, not to every reader: the decoded `Area` keeps `kind`
 * required so no consumer has to re-derive it.
 */
export const areaWireSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  kind: areaKindSchema.default('user'),
  // Historical `area.updated` payloads predate both creation preferences.
  // Current API snapshots emit explicit nulls; the decoder owns replay
  // compatibility so every domain consumer still sees required fields.
  default_template_id: z.string().nullable().default(null),
  default_cwd: z.string().nullable().default(null),
  created_at: z.number(),
  updated_at: z.number(),
});
export type AreaWire = z.infer<typeof areaWireSchema>;

export type Area = Readonly<{
  id: string;
  name: string;
  color: string;
  sort: number;
  kind: AreaKind;
  defaultTemplateId: string | null;
  defaultCwd: string | null;
  createdAt: number;
  updatedAt: number;
}>;

export function toArea(wire: AreaWire): Area {
  return {
    id: wire.id,
    name: wire.name,
    color: wire.color,
    sort: wire.sort,
    kind: wire.kind,
    defaultTemplateId: wire.default_template_id,
    defaultCwd: wire.default_cwd,
    createdAt: wire.created_at,
    updatedAt: wire.updated_at,
  };
}

export function areaListOperation(): ApiOperation<AreaWire[]> {
  return { method: 'GET', path: '/api/areas', responseSchema: z.array(areaWireSchema) };
}

/**
 * E2E-INV-SHELL-003 — the system area hosting the default Today terminal must
 * never reach a user-visible surface. `GET /api/areas` already filters it
 * server-side; this is the second layer of defence (#175) so a future
 * `?include_system=true` call site, or a replayed payload, cannot leak kernel
 * scaffolding into the sidebar or into Today's fan-out.
 */
export function visibleAreas(areas: readonly Area[]): Area[] {
  return areas.filter((area) => area.kind === 'user');
}

export function areaOf(areaId: string, areas: readonly Area[]): Area | undefined {
  return areas.find((area) => area.id === areaId);
}

export function sortedAreas(areas: readonly Area[]): Area[] {
  return [...areas].sort((left, right) => (left.sort !== right.sort
    ? left.sort - right.sort
    : left.id < right.id ? -1 : left.id > right.id ? 1 : 0));
}

export type NewAreaBody = Readonly<{
  name: string;
  color: string;
  sort?: number;
  default_template_id?: string | null;
  default_cwd?: string | null;
}>;
export type AreaPatchBody = Readonly<{
  name?: string;
  color?: string;
  sort?: number;
  default_template_id?: string | null;
  default_cwd?: string | null;
}>;

export function createAreaOperation(body: NewAreaBody): ApiOperation<AreaWire> {
  return { method: 'POST', path: '/api/areas', body, responseSchema: areaWireSchema };
}

export function updateAreaOperation(areaId: string, body: AreaPatchBody): ApiOperation<AreaWire> {
  return { method: 'PATCH', path: `/api/areas/${encodeURIComponent(areaId)}`, body, responseSchema: areaWireSchema };
}

export function deleteAreaOperation(areaId: string): ApiOperation<undefined> {
  return { method: 'DELETE', path: `/api/areas/${encodeURIComponent(areaId)}`, responseSchema: z.undefined() };
}

/**
 * A folder an area has claimed. The kernel's `area_folders` mapping, decoded.
 *
 * `id` is an autoincrement integer here rather than the usual uuid-shaped TEXT
 * because the row never enters the sync engine's event log — that is the
 * kernel's reason (see `AreaFolder` in `core/api/generated/wire.ts`), repeated
 * here only so a reader does not "fix" the type.
 *
 * `repo_identity` / `repo_identity_probed_at` are `null` until the kernel has
 * probed the folder's Git origin, so they stay nullable through the decode
 * rather than being defaulted into a lie.
 */
export const areaFolderWireSchema = z.object({
  id: z.number(),
  area_id: z.string(),
  path: z.string(),
  repo_identity: z.string().nullable().default(null),
  repo_identity_probed_at: z.number().nullable().default(null),
  created_at: z.number(),
});
export type AreaFolderWire = z.infer<typeof areaFolderWireSchema>;

export type AreaFolder = Readonly<{
  id: number;
  areaId: string;
  path: string;
  repoIdentity: string | null;
  repoIdentityProbedAt: number | null;
  createdAt: number;
}>;

export function toAreaFolder(wire: AreaFolderWire): AreaFolder {
  return {
    id: wire.id,
    areaId: wire.area_id,
    path: wire.path,
    repoIdentity: wire.repo_identity,
    repoIdentityProbedAt: wire.repo_identity_probed_at,
    createdAt: wire.created_at,
  };
}

export function areaFoldersOperation(areaId: string): ApiOperation<AreaFolderWire[]> {
  return {
    method: 'GET',
    path: `/api/areas/${encodeURIComponent(areaId)}/folders`,
    responseSchema: z.array(areaFolderWireSchema),
  };
}

/**
 * `path` ascending, ties broken by `id`. The kernel returns insertion order,
 * which is neither stable across a re-claim nor a useful display order.
 */
export function sortedAreaFolders(folders: readonly AreaFolder[]): AreaFolder[] {
  return [...folders].sort((left, right) => (left.path !== right.path
    ? (left.path < right.path ? -1 : 1)
    : left.id - right.id));
}

/**
 * The structured body `POST /api/tracks` answers a folder clash with (#275,
 * `area_folder_claim.rs`). It carries **no `error` key**, so the generic
 * failure normaliser in `core/api/client.ts` can only report the bare status
 * text — "Conflict" — and the reader is left with no idea which path or which
 * area is in the way. Decoding it is therefore not a nicety: it is the only
 * way this failure says anything at all.
 */
export const folderConflictSchema = z.object({
  folder_id: z.number(),
  area_id: z.string(),
  conflict_path: z.string(),
  conflict_kind: z.enum(['equal', 'ancestor', 'descendant']),
});
export type FolderConflict = z.infer<typeof folderConflictSchema>;

/** `null` for any other error body; the caller falls back to its own wording. */
export function asFolderConflict(body: unknown): FolderConflict | null {
  const parsed = folderConflictSchema.safeParse(body);
  return parsed.success ? parsed.data : null;
}

/**
 * The sentence a human can act on. `areaName` is `null` when the conflicting
 * area is not in the reader's area list — it may have been created in another
 * tab, or deleted between the conflict and this render — and the phrasing then
 * degrades to "another area" rather than printing a uuid.
 *
 * The three kinds are three different problems and get three different
 * remedies: `descendant` means somebody already owns this path, `ancestor`
 * means claiming it would silently widen a narrower claim underneath it, and
 * `equal` means this exact path is already claimed.
 */
export function folderConflictMessage(conflict: FolderConflict, areaName: string | null): string {
  const owner = areaName === null ? 'another area' : `area “${areaName}”`;
  switch (conflict.conflict_kind) {
    case 'descendant':
      return `That folder is already claimed by ${owner} (${conflict.conflict_path}). `
        + 'Start the track in that area, or pick a different folder.';
    case 'ancestor':
      return `A narrower claim under ${conflict.conflict_path} (owned by ${owner}) blocks claiming `
        + 'this folder. Remove the inner claim first, or pick a different folder.';
    case 'equal':
      return `That exact folder is already claimed by ${owner} (${conflict.conflict_path}).`;
  }
}

/** The eight identity slots. An area's colour is a slot, never a free hex (§6.2). */
export const AREA_SLOT_COUNT = 8;

/**
 * §6.2 — an area's identity dot is a stable hash of its id, mod 8, not the
 * kernel's `color` field. Two consequences the design leans on: the same area
 * is the same colour on every surface and across reloads, and the palette stays
 * inside the token set, so the eight hues can be re-tuned (or cut to six) in
 * `tokens.css` without touching a component.
 *
 * It lives in core rather than beside AREA_PALETTE because three separate
 * surfaces need it and `features/**` may not import a sibling feature domain —
 * and because "which slot is this area" is domain logic, not a palette value.
 */
export function areaSlotVar(areaId: string): string {
  let hash = 0;
  for (let index = 0; index < areaId.length; index += 1) {
    hash = (hash * 31 + areaId.charCodeAt(index)) | 0;
  }
  return `--area-${(Math.abs(hash) % AREA_SLOT_COUNT) + 1}`;
}
