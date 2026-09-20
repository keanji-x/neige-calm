// Area: the workspace grouping a track belongs to. Wire decode + the pure helpers every end shares.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const areaKindSchema = z.enum(['user', 'system']);
export type AreaKind = z.infer<typeof areaKindSchema>;

/** The default belongs to the decoder: the decoded `Area` keeps `kind` required so no consumer re-derives it. */
export const areaWireSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  kind: areaKindSchema.default('user'),
  // Historical `area.updated` payloads predate both creation preferences.
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

/**
 * Chooses the newest authoritative snapshot of one Area. Equal versions deliberately accept
 * the incoming carrier: historical event logs can contain same-millisecond updates.
 */
export function newestArea(current: Area, incoming: Area): Area {
  return current.updatedAt > incoming.updatedAt ? current : incoming;
}

export function areaListOperation(): ApiOperation<AreaWire[]> {
  return { method: 'GET', path: '/api/areas', responseSchema: z.array(areaWireSchema) };
}

/** The system area must never reach a user-visible surface; this is the client-side half of the server filter. */
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

/** Old servers omit this capability and must never receive a keyed create. */
export function areaCreationCapabilityOperation(): ApiOperation<'supported' | 'unsupported'> {
  return {
    method: 'GET', path: '/api/version',
    responseSchema: z.object({ areaCreateIdempotency: z.unknown().optional() })
      .transform((value) => value.areaCreateIdempotency === true ? 'supported' as const : 'unsupported' as const),
  };
}

/** One key identifies one exact creation intent, including every retry. */
export function createAreaOperation(body: NewAreaBody, idempotencyKey: string): ApiOperation<AreaWire> {
  return { method: 'POST', path: '/api/areas', body, headers: { 'Idempotency-Key': idempotencyKey }, responseSchema: areaWireSchema };
}

export function updateAreaOperation(areaId: string, body: AreaPatchBody): ApiOperation<AreaWire> {
  return { method: 'PATCH', path: `/api/areas/${encodeURIComponent(areaId)}`, body, responseSchema: areaWireSchema };
}

export function deleteAreaOperation(areaId: string): ApiOperation<undefined> {
  return { method: 'DELETE', path: `/api/areas/${encodeURIComponent(areaId)}`, responseSchema: z.undefined() };
}

/** `id` is an autoincrement integer, not a uuid, because the row never enters the sync engine's event log. */
export const areaFolderWireSchema = z.object({
  id: z.number(),
  area_id: z.string(),
  path: z.string(),
  created_at: z.number(),
});
export type AreaFolderWire = z.infer<typeof areaFolderWireSchema>;

export type AreaFolder = Readonly<{
  id: number;
  areaId: string;
  path: string;
  createdAt: number;
}>;

export function toAreaFolder(wire: AreaFolderWire): AreaFolder {
  return {
    id: wire.id,
    areaId: wire.area_id,
    path: wire.path,
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

/** `path` ascending, ties broken by `id`; the kernel returns insertion order. */
export function sortedAreaFolders(folders: readonly AreaFolder[]): AreaFolder[] {
  return [...folders].sort((left, right) => (left.path !== right.path
    ? (left.path < right.path ? -1 : 1)
    : left.id - right.id));
}

/**
 * The folder-clash body of `POST /api/tracks`. It carries no `error` key, so the generic failure
 * normaliser only reports "Conflict".
 */
export const folderConflictSchema = z.object({
  folder_id: z.number().int().safe().positive(),
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
 * `areaName` is `null` when the conflicting area is not in the reader's list; the phrasing then
 * degrades to "another area".
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

/** The eight identity slots. An area's colour is a slot, never a free hex. */
export const AREA_SLOT_COUNT = 8;

/** An area's identity dot is a stable hash of its id, mod 8, not the kernel's `color` field. */
export function areaSlotVar(areaId: string): string {
  let hash = 0;
  for (let index = 0; index < areaId.length; index += 1) {
    hash = (hash * 31 + areaId.charCodeAt(index)) | 0;
  }
  return `--area-${(Math.abs(hash) % AREA_SLOT_COUNT) + 1}`;
}
