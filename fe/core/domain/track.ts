// Track: the unit of work the product is organised around — wire decode, the
// lifecycle vocabulary, and the pure predicates several surfaces must agree on.

import { z } from 'zod';

import { cardRuntimeViewSchema } from '../api/schemas.js';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import {
  activityStateOf, type ActivityItem, type ActivityState, type AttentionKind, type CardActivity,
} from './activity.js';
import { visibleAreas, type Area } from './area.js';

export const trackLifecycleSchema = z.enum([
  'draft', 'planning', 'dispatching', 'working',
  'blocked', 'reviewing', 'done', 'canceled', 'failed',
]);
export type TrackLifecycle = z.infer<typeof trackLifecycleSchema>;

/**
 * `lifecycle` / `cwd` / the `*_at` columns are `#[serde(default)]` on the kernel side and so
 * absent from the OpenAPI `required` set; the decoder supplies the DB defaults.
 */
export const trackWireSchema = z.object({
  id: z.string(),
  area_id: z.string(),
  title: z.string(),
  sort: z.number(),
  lifecycle: trackLifecycleSchema.default('draft'),
  cwd: z.string().default(''),
  archived_at: z.number().nullable().default(null),
  pinned_at: z.number().nullable().default(null),
  terminal_at: z.number().nullable().default(null),
  created_at: z.number(),
  updated_at: z.number(),
});
export type TrackWire = z.infer<typeof trackWireSchema>;

/** Plugin-written activity on top of the kernel row; the neutral values mean "nothing posted", not "unknown". */
export type TrackActivity = Readonly<{
  progress: number;
  eta: string;
  now: string;
  /** From the kernel `activity` overlay: something dispatched is still running. */
  working: boolean;
  /** Same overlay: the fold of `attentionItems` — any failed → failed, else any input → input. */
  attention: AttentionKind;
  /** Same overlay: high-water mark of completion-class evidence; the read receipt compares against it. */
  activityAt: number | null;
  /** Same overlay: every item that needs a person, with where it came from. */
  attentionItems: readonly ActivityItem[];
  /** Same overlay: the per-card verdicts, keyed by card id. Read through `cardActivityOf`. */
  cards: Readonly<Record<string, CardActivity>>;
}>;

/* Nested containers are frozen too: `no-module-runtime-state` only credits a fully frozen literal,
 * and a `new Map()` here would be rejected — hence `cards` is a `Record`. */
export const NEUTRAL_ACTIVITY: TrackActivity = Object.freeze({
  progress: 0, eta: '', now: '',
  working: false, attention: 'none', activityAt: null,
  attentionItems: Object.freeze([]), cards: Object.freeze({}),
});

export type Track = Readonly<{
  id: string;
  areaId: string;
  title: string;
  sort: number;
  lifecycle: TrackLifecycle;
  cwd: string;
  archivedAt: number | null;
  pinnedAt: number | null;
  terminalAt: number | null;
  createdAt: number;
  updatedAt: number;
}> & TrackActivity;

export function toTrack(wire: TrackWire, activity: TrackActivity = NEUTRAL_ACTIVITY): Track {
  return {
    id: wire.id,
    areaId: wire.area_id,
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

/**
 * Resolves a report's `neige://plugin/<plugin_id>/<kind>` source to at most one overlay's
 * payload, unvalidated (the renderer decodes it); `undefined` when nothing matches.
 */
export function trackOverlayPayload(
  trackId: string,
  overlays: readonly OverlayWire[],
  source: string,
): unknown {
  const rest = source.startsWith(LIVE_TABLE_SOURCE_PREFIX)
    ? source.slice(LIVE_TABLE_SOURCE_PREFIX.length)
    : null;
  if (rest === null) return undefined;
  const slash = rest.indexOf('/');
  if (slash <= 0 || slash === rest.length - 1) return undefined;
  const pluginId = rest.slice(0, slash);
  const kind = rest.slice(slash + 1);
  // `indexOf` above splits at the FIRST slash, so a three-segment source would
  // otherwise resolve as a kind containing a slash. Overlay kinds never do.
  if (kind.includes('/')) return undefined;
  const match = overlays.find((overlay) => overlay.entity_kind === 'track'
    && overlay.entity_id === trackId
    && overlay.plugin_id === pluginId
    && overlay.kind === kind);
  return match?.payload;
}

/** Scheme prefix of a live table `source`. Mirrors `report_blocks::LIVE_SOURCE_PREFIX`. */
const LIVE_TABLE_SOURCE_PREFIX = 'neige://plugin/';

function payloadField(payload: unknown, key: string): unknown {
  return typeof payload === 'object' && payload !== null
    ? (payload as Record<string, unknown>)[key]
    : undefined;
}

const attentionKindSchema = z.enum(['none', 'input', 'failed']);
const activityItemWireSchema = z.object({
  kind: z.enum(['input', 'failed']),
  source: z.enum(['card', 'task', 'session', 'lifecycle']),
  id: z.string(),
  card_id: z.string().nullable(),
  at_ms: z.number(),
});
const activityCardWireSchema = z.object({
  card_id: z.string(),
  state: z.enum(['working', 'input', 'failed']),
});

/** Mirrors `calm_truth::validation::KERNEL_OVERLAY_PLUGIN_ID`. */
const KERNEL_OVERLAY_PLUGIN_ID = 'kernel';
const activityOverlayWireSchema = z.object({
  schemaVersion: z.literal(1),
  working: z.boolean(),
  attention: attentionKindSchema,
  activity_at_ms: z.number().nullable(),
  items: z.array(z.unknown()),
  cards: z.array(z.unknown()),
});

function activityOverlayFields(payload: unknown): Partial<TrackActivity> | null {
  const parsed = activityOverlayWireSchema.safeParse(payload);
  if (!parsed.success) return null;
  const attentionItems: ActivityItem[] = [];
  for (const row of parsed.data.items) {
    const item = activityItemWireSchema.safeParse(row);
    if (!item.success) continue;
    attentionItems.push({
      origin: item.data.source, id: item.data.id, cardId: item.data.card_id,
      atMs: item.data.at_ms, kind: item.data.kind,
    });
  }
  const cards: Record<string, CardActivity> = {};
  for (const row of parsed.data.cards) {
    const card = activityCardWireSchema.safeParse(row);
    if (card.success) cards[card.data.card_id] = card.data.state;
  }
  return {
    working: parsed.data.working,
    attention: parsed.data.attention,
    activityAt: parsed.data.activity_at_ms,
    attentionItems,
    cards,
  };
}

/**
 * Folds a track's overlays into its activity fields; junk payloads are ignored, not rejected.
 * Only the kernel-written `activity` row is the verdict — any plugin may write a row of any kind under its own id.
 */
export function trackActivityFrom(trackId: string, overlays: readonly OverlayWire[]): TrackActivity {
  let activity = NEUTRAL_ACTIVITY;
  for (const overlay of overlays) {
    if (overlay.entity_kind !== 'track' || overlay.entity_id !== trackId) continue;
    const value = payloadField(overlay.payload, 'value');
    const text = payloadField(overlay.payload, 'text');
    if (overlay.kind === 'progress' && typeof value === 'number') activity = { ...activity, progress: value };
    else if (overlay.kind === 'eta' && typeof text === 'string') activity = { ...activity, eta: text };
    else if (overlay.kind === 'now' && typeof text === 'string') activity = { ...activity, now: text };
    else if (overlay.kind === 'activity' && overlay.plugin_id === KERNEL_OVERLAY_PLUGIN_ID) {
      const fields = activityOverlayFields(overlay.payload);
      if (fields !== null) activity = { ...activity, ...fields };
    }
  }
  return activity;
}

/** The longest title the Notifications aside takes from a card's goal. */
export const CARD_GOAL_TITLE_MAX = 60;

/**
 * The first line of a card's `payload.goal`, capped at `CARD_GOAL_TITLE_MAX` characters — how the
 * Notifications aside names a worker card. `Card.payload` is `z.unknown()`, so this is a narrow
 * runtime guard (an object with a string `goal`), not a schema; `null` when there is no such goal.
 */
export function cardGoalTitle(payload: unknown): string | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const goal = (payload as { goal?: unknown }).goal;
  if (typeof goal !== 'string') return null;
  const newline = goal.indexOf('\n');
  const line = (newline === -1 ? goal : goal.slice(0, newline)).trim();
  if (line === '') return null;
  const chars = [...line];
  return chars.length > CARD_GOAL_TITLE_MAX ? `${chars.slice(0, CARD_GOAL_TITLE_MAX - 1).join('')}…` : line;
}

export const cardWireSchema = z.object({
  id: z.string(),
  track_id: z.string(),
  kind: z.string(),
  title: z.string().nullable().default(null),
  sort: z.number(),
  payload: z.unknown(),
  deletable: z.boolean().default(true),
  runtime: cardRuntimeViewSchema.optional(),
  created_at: z.number(),
  updated_at: z.number(),
});
export type CardWire = z.infer<typeof cardWireSchema>;

export const trackDetailSchema = z.object({
  track: trackWireSchema,
  can_resume: z.boolean(),
  cards: z.array(cardWireSchema),
  overlays: z.array(overlayWireSchema),
});
export type TrackDetailWire = z.infer<typeof trackDetailSchema>;

/** `{ fg, bg }` RGB the kernel stamps onto a spawning daemon's argv. */
export type ThemeRgb = Readonly<{ fg: readonly [number, number, number]; bg: readonly [number, number, number] }>;

/**
 * Matches the kernel's `str::trim().is_empty()`: Rust whitespace is Unicode `White_Space`, which
 * JS `trim()` is not (`U+0085`). Answers only "is this blank" — the value itself is never trimmed.
 */
export function isBlankForKernel(text: string): boolean {
  return /^\p{White_Space}*$/u.test(text);
}

export type NewTrackBody = Readonly<{
  area_id: string;
  /** Planner overrides applied before the first message; omitted follows installation defaults. */
  model?: string;
  reasoning_effort?: string;
  /** Optional; omitted stores the empty string (the kernel has no default name). Present values, including `""`, are stored verbatim. */
  title?: string;
  /** Omitted (or `null`) means a managed workspace the server allocates beneath its workspace root. */
  cwd?: string | null;
  theme: ThemeRgb;
  /**
   * `false` requires `cwd` to already sit under a claimed folder (409 naming the area otherwise);
   * `true` claims it in the same transaction. Omitting `cwd` forces `false`.
   */
  attach_folder?: boolean;
  /** Single-use consent for the exact foreign folder claim returned by a
   * create 409. The kernel revalidates both ids transactionally. */
  allow_cross_area_cwd?: Readonly<{ folder_id: number; area_id: string }>;
  /** The chosen template's key (`template.id` from `GET /api/track-templates`). Blank omits the key entirely: the kernel 400s a whitespace-only id. */
  template_id?: string;
  /** Only accepted when `GET /api/track-templates` returned an `input_schema` for the template; otherwise a 400. */
  template_input?: Readonly<Record<string, unknown>>;
  /** The chosen user recipe's id. Mutually exclusive with `template_id` (400 naming both). Absent, never `''`. */
  recipe_id?: string;
  /**
   * The reader's first sentence, seeded to the planner by this create. Blank per `isBlankForKernel`
   * omits the key; a sent value goes verbatim, untrimmed. Present ⇒ `Idempotency-Key` is required.
   */
  first_message?: string;
}>;

/** The two legal request-body shapes at the operation boundary. */
export type NewTrackBodyWithoutFirstMessage = Omit<NewTrackBody, 'first_message'> & Readonly<{
  first_message?: undefined;
}>;
export type NewTrackBodyWithFirstMessage = Omit<NewTrackBody, 'first_message'> & Readonly<{
  first_message: string;
}>;

/** A selectable starting point for a new track. `input_schema` is present exactly when a running trusted plugin is bound and the template takes input. */
export const trackTemplateSchema = z.object({
  id: z.string(),
  title: z.string(),
  input_schema: z.unknown().optional(),
  /** Required, not `.default([])`: a default would let a broken read render as "pre-sets nothing". */
  tasks: z.array(z.object({ key: z.string(), goal: z.string() })),
});
export type TrackTemplate = z.infer<typeof trackTemplateSchema>;

export function trackTemplatesOperation(): ApiOperation<TrackTemplate[]> {
  return { method: 'GET', path: '/api/track-templates', responseSchema: z.array(trackTemplateSchema) };
}

/** A user-defined starting point for a new track — a `track_recipes` row. */
export const trackRecipeSchema = z.object({
  id: z.string(),
  /** Picker label *and* the instantiated report's summary — one field on the
   *  kernel side too, so the editor edits one thing. */
  title: z.string(),
  /** The report body. Its `neige-block` fences are the recipe's tasks. */
  body: z.string(),
  /** Optimistic-lock anchor a `PUT` must echo as `if_revision`. */
  revision: z.number(),
  created_at: z.number(),
  updated_at: z.number(),
});
export type TrackRecipe = z.infer<typeof trackRecipeSchema>;

export function trackRecipesOperation(): ApiOperation<TrackRecipe[]> {
  return { method: 'GET', path: '/api/track-recipes', responseSchema: z.array(trackRecipeSchema) };
}

export function createTrackRecipeOperation(
  body: Readonly<{ title: string; body: string }>,
): ApiOperation<TrackRecipe> {
  return { method: 'POST', path: '/api/track-recipes', body, responseSchema: trackRecipeSchema };
}

/**
 * Whole-document replace gated on `revision`. The response is the stored row, which may differ
 * from the bytes sent (fences re-rendered, tombstones dropped): render what comes back.
 */
export function updateTrackRecipeOperation(
  recipeId: string,
  body: Readonly<{ title: string; body: string; if_revision: number }>,
): ApiOperation<TrackRecipe> {
  return {
    method: 'PUT',
    path: `/api/track-recipes/${encodeURIComponent(recipeId)}`,
    body,
    responseSchema: trackRecipeSchema,
  };
}

export function deleteTrackRecipeOperation(recipeId: string): ApiOperation<undefined> {
  return {
    method: 'DELETE',
    path: `/api/track-recipes/${encodeURIComponent(recipeId)}`,
    responseSchema: z.undefined(),
  };
}

export type TrackPatchBody = Readonly<{
  title?: string;
  sort?: number;
  pinned_at?: number | null;
  archived_at?: number | null;
  lifecycle?: TrackLifecycle;
}>;

export function tracksInAreaOperation(areaId: string): ApiOperation<TrackWire[]> {
  return {
    method: 'GET',
    path: `/api/areas/${encodeURIComponent(areaId)}/tracks`,
    responseSchema: z.array(trackWireSchema),
  };
}

export function trackDetailOperation(trackId: string): ApiOperation<TrackDetailWire> {
  return { method: 'GET', path: `/api/tracks/${encodeURIComponent(trackId)}`, responseSchema: trackDetailSchema };
}

/**
 * `POST /api/tracks`. The kernel requires `Idempotency-Key` whenever `first_message` is present;
 * mint it per draft, not per call, or a retry mints a second track holding the same message.
 */
export function createTrackOperation(body: NewTrackBodyWithoutFirstMessage): ApiOperation<TrackWire>;
export function createTrackOperation(
  body: NewTrackBodyWithFirstMessage,
  idempotencyKey: string,
): ApiOperation<TrackWire>;
export function createTrackOperation(body: NewTrackBody, idempotencyKey?: string): ApiOperation<TrackWire> {
  if (body.first_message !== undefined && idempotencyKey === undefined) {
    throw new TypeError('Idempotency-Key is required when first_message is present');
  }
  return {
    method: 'POST',
    path: '/api/tracks',
    body,
    responseSchema: trackWireSchema,
    ...(body.first_message === undefined || idempotencyKey === undefined
      ? {}
      : { headers: { 'Idempotency-Key': idempotencyKey } }),
  };
}

export type TrackCreateKeyAction = 'preserve' | 'replace' | 'offer-explicit-replace';

/**
 * Only `idempotency_key_exhausted` earns a fresh key: transport errors and 5xx may have committed,
 * so rotating their key could mint a second track. Payload conflicts expose an explicit choice.
 */
export function trackCreateKeyAction(failure: ApiFailure): TrackCreateKeyAction {
  if (failure.kind !== 'http') return 'preserve';
  if (failure.code === 'idempotency_key_exhausted') return 'replace';
  if (failure.code === 'conflict' && (
    failure.message.includes('already used with different payload')
    || failure.message.includes('predates durable request fingerprints')
  )) {
    return 'offer-explicit-replace';
  }
  return 'preserve';
}

export function updateTrackOperation(trackId: string, body: TrackPatchBody): ApiOperation<TrackWire> {
  return { method: 'PATCH', path: `/api/tracks/${encodeURIComponent(trackId)}`, body, responseSchema: trackWireSchema };
}

export function deleteTrackOperation(trackId: string): ApiOperation<undefined> {
  return { method: 'DELETE', path: `/api/tracks/${encodeURIComponent(trackId)}`, responseSchema: z.undefined() };
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
  trackId: string,
  body: NewTerminalCardBody,
): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/terminal-cards`,
    body,
    responseSchema: cardWireSchema,
  };
}

/** `theme` is required by the kernel (422 without it): the daemon answers codex's OSC 10/11 probe with these colours. */
export type NewCodexCardBody = Readonly<{
  theme: ThemeRgb;
  title?: string | null;
  cwd?: string;
  prompt?: string;
  sort?: number | null;
}>;

export function createCodexCardOperation(
  trackId: string,
  body: NewCodexCardBody,
): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/codex-cards`,
    body,
    responseSchema: cardWireSchema,
  };
}

/** `POST /api/tracks/:id/cards` — direct create for kinds that own no runtime; worker kinds have atomic endpoints of their own. */
export type NewCardBody = Readonly<{
  kind: string;
  payload?: unknown;
  title?: string | null;
  sort?: number | null;
}>;

export function createCardOperation(trackId: string, body: NewCardBody): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/cards`,
    body,
    responseSchema: cardWireSchema,
  };
}

/** The kernel refuses this for a card it owns (`deletable === false`). */
export function deleteCardOperation(cardId: string): ApiOperation<undefined> {
  return {
    method: 'DELETE',
    path: `/api/cards/${encodeURIComponent(cardId)}`,
    responseSchema: z.undefined(),
  };
}

export function overlaysByKindOperation(entityKind: 'track' | 'card'): ApiOperation<OverlayWire[]> {
  return {
    method: 'GET',
    path: `/api/overlays?entity_kind=${entityKind}`,
    responseSchema: z.array(overlayWireSchema),
  };
}

/** The track needs a human: blocked, in review, or failed. */
export function isWaitingForUser(lifecycle: TrackLifecycle): boolean {
  return lifecycle === 'blocked' || lifecycle === 'reviewing' || lifecycle === 'failed';
}

/* The three activity predicates read only the kernel's `activity` overlay — no lifecycle OR — so they cannot disagree with it. */

/** The kernel says something dispatched is still running. */
export function isWorking(track: Track): boolean {
  return track.working;
}

/** The kernel says a person has to give input somewhere on this track. */
export function needsUserAttention(track: Track): boolean {
  return track.attention === 'input';
}

/** The kernel says something on this track is broken and needs repair. */
export function hasFailed(track: Track): boolean {
  return track.attention === 'failed';
}

/** The one indicator state for a track on every surface; `unread` is the reader's receipt, the rest is the overlay's. */
export function trackActivityState(track: Track, unread: boolean): ActivityState {
  return activityStateOf({ working: isWorking(track), attention: track.attention, unread });
}

/** Needs a person first, then in a running phase, then everything quiet. */
export function lifecycleRank(track: Track): number {
  if (needsUserAttention(track) || hasFailed(track)) return 0;
  if (isRunning(track.lifecycle)) return 1;
  return 2;
}

export function sortByLifecycleRank(tracks: readonly Track[]): Track[] {
  return [...tracks].sort((left, right) => lifecycleRank(left) - lifecycleRank(right));
}

/** Archived is an orthogonal visibility flag, never a lifecycle bucket. */
export function visibleTracks(tracks: readonly Track[]): Track[] {
  return tracks.filter((track) => track.archivedAt === null);
}

/** Not archived and hosted by an area the person may see — the second layer of defence behind `visibleAreas`. */
export function userVisibleTracks(tracks: readonly Track[], areas: readonly Area[]): Track[] {
  const userAreaIds = new Set(visibleAreas(areas).map((area) => area.id));
  return visibleTracks(tracks).filter((track) => userAreaIds.has(track.areaId));
}

/** The track has work in flight. `done` / `draft` / `canceled` are neither. */
export function isRunning(lifecycle: TrackLifecycle): boolean {
  return lifecycle === 'planning' || lifecycle === 'dispatching' || lifecycle === 'working';
}

export function isTerminal(lifecycle: TrackLifecycle): boolean {
  return lifecycle === 'done' || lifecycle === 'canceled' || lifecycle === 'failed';
}

export const UNTITLED_TRACK_LABEL = 'Untitled track';

/** One display fallback for tracks created without a title. */
export function trackDisplayTitle(title: string): string {
  return title.trim() || UNTITLED_TRACK_LABEL;
}

/** The canonical lifecycle phrase. Every surface reads it from here so the
 *  sidebar, the badge, and the agenda cannot drift into parallel tables. */
export function lifecycleLabel(lifecycle: TrackLifecycle): string {
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
 * Every track whose `[createdAt, terminalAt ?? nowMs]` interval overlaps the local day owning `day`;
 * endpoints inclusive, sorted by `createdAt` then id.
 */
export function activeTracksOn(tracks: readonly Track[], day: Date, nowMs: number): Track[] {
  const dayStart = startOfDay(day);
  const dayEnd = endOfDay(day);
  const matched = tracks.filter((track) => {
    const end = track.terminalAt ?? (isTerminal(track.lifecycle) ? track.updatedAt : nowMs);
    return track.createdAt <= dayEnd && end >= dayStart;
  });
  return matched.sort((left, right) => (left.createdAt !== right.createdAt
    ? left.createdAt - right.createdAt
    : left.id < right.id ? -1 : left.id > right.id ? 1 : 0));
}
