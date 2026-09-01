// Wave: the unit of work the whole product is organised around. Wire decode,
// the lifecycle vocabulary, and the pure predicates that several surfaces must
// agree on (sidebar buckets, Today's counters, Today's agenda).

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import { visibleCoves, type Cove } from './cove.js';

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

/**
 * Plugin-written activity a wave carries on top of its kernel row. The kernel
 * stores these as overlays, so a wave read without overlays still has to be a
 * complete `Wave` — the neutral values below are what "no plugin has posted
 * anything" means, not "unknown".
 */
export type WaveActivity = Readonly<{
  progress: number;
  eta: string;
  now: string;
  anyCardNeedsInput: boolean;
}>;

export const NEUTRAL_ACTIVITY: WaveActivity = Object.freeze({
  progress: 0, eta: '', now: '', anyCardNeedsInput: false,
});

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
}> & WaveActivity;

export function toWave(wire: WaveWire, activity: WaveActivity = NEUTRAL_ACTIVITY): Wave {
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
    ...activity,
  };
}

export const overlayWireSchema = z.object({
  id: z.string(),
  plugin_id: z.string(),
  entity_kind: z.string(),
  entity_id: z.string(),
  kind: z.string(),
  payload: z.unknown(),
  updated_at: z.number(),
});
export type OverlayWire = z.infer<typeof overlayWireSchema>;

function payloadField(payload: unknown, key: string): unknown {
  return typeof payload === 'object' && payload !== null
    ? (payload as Record<string, unknown>)[key]
    : undefined;
}

/**
 * Folds a wave's overlays into its activity fields. Unknown overlay kinds and
 * mistyped payloads are ignored rather than rejected: a plugin writing junk
 * must not blank out a wave the sidebar is trying to render.
 */
export function waveActivityFrom(waveId: string, overlays: readonly OverlayWire[]): WaveActivity {
  let activity = NEUTRAL_ACTIVITY;
  for (const overlay of overlays) {
    if (overlay.entity_kind !== 'wave' || overlay.entity_id !== waveId) continue;
    const value = payloadField(overlay.payload, 'value');
    const text = payloadField(overlay.payload, 'text');
    if (overlay.kind === 'progress' && typeof value === 'number') activity = { ...activity, progress: value };
    else if (overlay.kind === 'eta' && typeof text === 'string') activity = { ...activity, eta: text };
    else if (overlay.kind === 'now' && typeof text === 'string') activity = { ...activity, now: text };
    else if (overlay.kind === 'any_card_needs_input' && typeof value === 'boolean') {
      activity = { ...activity, anyCardNeedsInput: value };
    }
  }
  return activity;
}

export const cardWireSchema = z.object({
  id: z.string(),
  wave_id: z.string(),
  kind: z.string(),
  title: z.string().nullable().default(null),
  sort: z.number(),
  payload: z.unknown(),
  deletable: z.boolean().default(true),
  created_at: z.number(),
  updated_at: z.number(),
});
export type CardWire = z.infer<typeof cardWireSchema>;

export const waveDetailSchema = z.object({
  wave: waveWireSchema,
  cards: z.array(cardWireSchema),
  overlays: z.array(overlayWireSchema),
});
export type WaveDetailWire = z.infer<typeof waveDetailSchema>;

/** `{ fg, bg }` RGB the kernel stamps onto a spawning daemon's argv (#177). */
export type ThemeRgb = Readonly<{ fg: readonly [number, number, number]; bg: readonly [number, number, number] }>;

export type NewWaveBody = Readonly<{
  cove_id: string;
  /**
   * Issue #1211 — optional. The title is no longer the wave's intent: omit it
   * and the kernel stores its default name until the spec agent renames the
   * wave. Present values (including `""`) are still accepted verbatim.
   */
  title?: string;
  /**
   * Issue #1131 — optional. The new FE omits it; the kernel then stores
   * `$HOME` and does not insert a `cove_folders` row. Present values
   * (including `""`) keep the pre-#1131 absolute-path + claim rules.
   * `null` matches OpenAPI (`string | null`) and is the same omitted branch.
   */
  cwd?: string | null;
  theme: ThemeRgb;
  /**
   * `false` requires `cwd` to already sit under a folder claimed by some cove;
   * the route answers 409 `conflict` naming the cove to claim it for. `true`
   * claims it in the same transaction. Omitting `cwd` forces this to `false`
   * on the kernel regardless of the field. When `cwd` is present, omitting
   * `attach_folder` is `false`.
   */
  attach_folder?: boolean;
  /**
   * The chosen template's key (#1209). Read as `template.id` from
   * `GET /api/wave-templates`; the write side still spells it `workflow_id`
   * because on this field that name is accurate — it is what the kernel's
   * plugin-binding path resolves. #1209 records the seam and the decision not
   * to add a `template_id` alias.
   *
   * **Blank omits the key entirely.** Not `null`, not `''`: the kernel rejects
   * a whitespace-only id with a 400 and the body is
   * `deny_unknown_fields`-strict, so the only spelling of "no template" is
   * absence.
   */
  workflow_id?: string;
  /**
   * Only accepted when the chosen template is bound to a running trusted
   * plugin — i.e. exactly when `GET /api/wave-templates` returned an
   * `input_schema` for it. Sending it otherwise is a 400.
   */
  workflow_input?: Readonly<Record<string, unknown>>;
}>;

/**
 * A selectable starting point for a new wave (#1209).
 *
 * `input_schema` is present only when a running trusted plugin is bound to the
 * template; its presence — not the template's id — is what says "this one takes
 * input". Kept as `unknown`: the picker branches on presence, and nothing in
 * the FE evaluates JSON Schema.
 *
 * There is no `description`, on purpose. The kernel has no such fact (#1209
 * §"权威源散在三处"), so a description here would be a fourth authority for
 * what a template is. What the picker shows instead is `tasks` — the plan the
 * template *already* contains, surfaced rather than re-described.
 */
export const waveTemplateSchema = z.object({
  id: z.string(),
  title: z.string(),
  input_schema: z.unknown().optional(),
  /**
   * The tasks the template pre-sets, in plan order — the same `task` blocks
   * the created wave's report is seeded with.
   *
   * Required, not `.default([])`: the server sends it for every template, and
   * a default would let a genuinely broken read render as "this template
   * pre-sets nothing", which is a lie the user cannot tell from the truth.
   */
  tasks: z.array(z.object({ key: z.string(), goal: z.string() })),
});
export type WaveTemplate = z.infer<typeof waveTemplateSchema>;

export function waveTemplatesOperation(): ApiOperation<WaveTemplate[]> {
  return { method: 'GET', path: '/api/wave-templates', responseSchema: z.array(waveTemplateSchema) };
}

export type WavePatchBody = Readonly<{
  title?: string;
  sort?: number;
  pinned_at?: number | null;
  archived_at?: number | null;
}>;

export function wavesInCoveOperation(coveId: string): ApiOperation<WaveWire[]> {
  return {
    method: 'GET',
    path: `/api/coves/${encodeURIComponent(coveId)}/waves`,
    responseSchema: z.array(waveWireSchema),
  };
}

export function waveDetailOperation(waveId: string): ApiOperation<WaveDetailWire> {
  return { method: 'GET', path: `/api/waves/${encodeURIComponent(waveId)}`, responseSchema: waveDetailSchema };
}

export function createWaveOperation(body: NewWaveBody): ApiOperation<WaveWire> {
  return { method: 'POST', path: '/api/waves', body, responseSchema: waveWireSchema };
}

export function updateWaveOperation(waveId: string, body: WavePatchBody): ApiOperation<WaveWire> {
  return { method: 'PATCH', path: `/api/waves/${encodeURIComponent(waveId)}`, body, responseSchema: waveWireSchema };
}

export function deleteWaveOperation(waveId: string): ApiOperation<undefined> {
  return { method: 'DELETE', path: `/api/waves/${encodeURIComponent(waveId)}`, responseSchema: z.undefined() };
}

export type NewTerminalCardBody = Readonly<{
  theme: ThemeRgb;
  cwd?: string;
  program?: string;
  title?: string | null;
  sort?: number | null;
  env?: Readonly<Record<string, string>>;
}>;

export function createTerminalCardOperation(
  waveId: string,
  body: NewTerminalCardBody,
): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/waves/${encodeURIComponent(waveId)}/terminal-cards`,
    body,
    responseSchema: cardWireSchema,
  };
}

/**
 * `POST /api/waves/:id/codex-cards` — the atomic codex spawn. `theme` is
 * required by the kernel (422 without it): the daemon answers codex's OSC 10/11
 * probe with these colours, so a card minted from a light host must not come up
 * painted for a dark one.
 */
export type NewCodexCardBody = Readonly<{
  theme: ThemeRgb;
  title?: string | null;
  cwd?: string;
  prompt?: string;
  sort?: number | null;
}>;

export function createCodexCardOperation(
  waveId: string,
  body: NewCodexCardBody,
): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/waves/${encodeURIComponent(waveId)}/codex-cards`,
    body,
    responseSchema: cardWireSchema,
  };
}

/**
 * `POST /api/waves/:id/cards` — the kernel's direct-create path: the row is
 * written verbatim from `kind` + `payload`. Only kinds that own no runtime may
 * take this door; a worker kind (terminal / codex / claude) has an atomic
 * endpoint of its own because the kernel has a daemon to spawn as well as a row
 * to write.
 */
export type NewCardBody = Readonly<{
  kind: string;
  payload?: unknown;
  title?: string | null;
  sort?: number | null;
}>;

export function createCardOperation(waveId: string, body: NewCardBody): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/waves/${encodeURIComponent(waveId)}/cards`,
    body,
    responseSchema: cardWireSchema,
  };
}

/**
 * `DELETE /api/cards/:id`. The kernel refuses this for a card it owns
 * (`deletable === false`), which is why every surface that offers the gesture
 * reads that bit first rather than discovering the refusal in an error toast.
 */
export function deleteCardOperation(cardId: string): ApiOperation<undefined> {
  return {
    method: 'DELETE',
    path: `/api/cards/${encodeURIComponent(cardId)}`,
    responseSchema: z.undefined(),
  };
}

export function overlaysByKindOperation(entityKind: 'wave' | 'card'): ApiOperation<OverlayWire[]> {
  return {
    method: 'GET',
    path: `/api/overlays?entity_kind=${entityKind}`,
    responseSchema: z.array(overlayWireSchema),
  };
}

/** The wave needs a human: blocked, in review, or failed. */
export function isWaitingForUser(lifecycle: WaveLifecycle): boolean {
  return lifecycle === 'blocked' || lifecycle === 'reviewing' || lifecycle === 'failed';
}

/**
 * #254 — the UI grouping predicate for every "Waiting on you" surface. ORs the
 * lifecycle bucket with the kernel `card_fsm`-derived overlay so a wave whose
 * worker card is sitting on AwaitingInput surfaces even before the Spec Agent
 * has driven `working → blocked`.
 *
 * This stays separate from `isWaitingForUser` on purpose: the two signals have
 * different owners (Spec Agent vs kernel) and different storage (column vs
 * overlay), and places that genuinely want the pure lifecycle bucket — the
 * lifecycle badge, cove bucket sort — must keep getting it.
 */
export function needsUserAttention(wave: Wave): boolean {
  return isWaitingForUser(wave.lifecycle) || wave.anyCardNeedsInput;
}

/** Waiting first, then running, then everything quiet. */
export function lifecycleRank(wave: Wave): number {
  if (needsUserAttention(wave)) return 0;
  if (isRunning(wave.lifecycle)) return 1;
  return 2;
}

export function sortByLifecycleRank(waves: readonly Wave[]): Wave[] {
  return [...waves].sort((left, right) => lifecycleRank(left) - lifecycleRank(right));
}

/** Archived is an orthogonal visibility flag, never a lifecycle bucket. */
export function visibleWaves(waves: readonly Wave[]): Wave[] {
  return waves.filter((wave) => wave.archivedAt === null);
}

/**
 * The waves a person may see: not archived, and hosted by a cove they may see.
 *
 * This is **not** a fix for a live leak — `coveListQueryOptions` already applies
 * `visibleCoves` in the query layer, and the workspace only fans out over what
 * that returned. It is the *second* layer of defence `visibleCoves` announces
 * (E2E-INV-SHELL-003), and it existed on exactly one of the two list surfaces:
 * the sidebar intersected coves and waves by hand while mobile Pages filtered
 * waves alone. One function, used by both, is what makes the stated intent true
 * at the component boundary rather than only in the query that happens to feed
 * it today (#1191 §3.1).
 */
export function userVisibleWaves(waves: readonly Wave[], coves: readonly Cove[]): Wave[] {
  const userCoveIds = new Set(visibleCoves(coves).map((cove) => cove.id));
  return visibleWaves(waves).filter((wave) => userCoveIds.has(wave.coveId));
}

/** The wave has work in flight. `done` / `draft` / `canceled` are neither. */
export function isRunning(lifecycle: WaveLifecycle): boolean {
  return lifecycle === 'planning' || lifecycle === 'dispatching' || lifecycle === 'working';
}

export function isTerminal(lifecycle: WaveLifecycle): boolean {
  return lifecycle === 'done' || lifecycle === 'canceled' || lifecycle === 'failed';
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
    const end = wave.terminalAt ?? (isTerminal(wave.lifecycle) ? wave.updatedAt : nowMs);
    return wave.createdAt <= dayEnd && end >= dayStart;
  });
  return matched.sort((left, right) => (left.createdAt !== right.createdAt
    ? left.createdAt - right.createdAt
    : left.id < right.id ? -1 : left.id > right.id ? 1 : 0));
}
