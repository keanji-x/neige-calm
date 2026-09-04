// Track: the unit of work the whole product is organised around. Wire decode,
// the lifecycle vocabulary, and the pure predicates that several surfaces must
// agree on (sidebar buckets, Today's counters, Today's agenda).

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import { visibleAreas, type Area } from './area.js';

export const trackLifecycleSchema = z.enum([
  'draft', 'planning', 'dispatching', 'working',
  'blocked', 'reviewing', 'done', 'canceled', 'failed',
]);
export type TrackLifecycle = z.infer<typeof trackLifecycleSchema>;

/**
 * `lifecycle` / `cwd` / the `*_at` columns carry `#[serde(default)]` on the
 * kernel side for event-log replay, so they are absent from the OpenAPI
 * `required` set. The decoder supplies the documented DB defaults and the
 * decoded `Track` keeps every field required.
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

/**
 * Plugin-written activity a track carries on top of its kernel row. The kernel
 * stores these as overlays, so a track read without overlays still has to be a
 * complete `Track` — the neutral values below are what "no plugin has posted
 * anything" means, not "unknown".
 */
export type TrackActivity = Readonly<{
  progress: number;
  eta: string;
  now: string;
  anyCardNeedsInput: boolean;
}>;

export const NEUTRAL_ACTIVITY: TrackActivity = Object.freeze({
  progress: 0, eta: '', now: '', anyCardNeedsInput: false,
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

function payloadField(payload: unknown, key: string): unknown {
  return typeof payload === 'object' && payload !== null
    ? (payload as Record<string, unknown>)[key]
    : undefined;
}

/**
 * Folds a track's overlays into its activity fields. Unknown overlay kinds and
 * mistyped payloads are ignored rather than rejected: a plugin writing junk
 * must not blank out a track the sidebar is trying to render.
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
    else if (overlay.kind === 'any_card_needs_input' && typeof value === 'boolean') {
      activity = { ...activity, anyCardNeedsInput: value };
    }
  }
  return activity;
}

export const cardWireSchema = z.object({
  id: z.string(),
  track_id: z.string(),
  kind: z.string(),
  title: z.string().nullable().default(null),
  sort: z.number(),
  payload: z.unknown(),
  deletable: z.boolean().default(true),
  created_at: z.number(),
  updated_at: z.number(),
});
export type CardWire = z.infer<typeof cardWireSchema>;

export const trackDetailSchema = z.object({
  track: trackWireSchema,
  cards: z.array(cardWireSchema),
  overlays: z.array(overlayWireSchema),
});
export type TrackDetailWire = z.infer<typeof trackDetailSchema>;

/** `{ fg, bg }` RGB the kernel stamps onto a spawning daemon's argv (#177). */
export type ThemeRgb = Readonly<{ fg: readonly [number, number, number]; bg: readonly [number, number, number] }>;

/**
 * "The kernel will read this text as blank" — the one place that question is
 * answered in the frontend (#1299).
 *
 * The kernel's blank check is `str::trim().is_empty()`
 * (`crates/calm-server/src/routes/conversations_shared.rs`,
 * `validate_first_message`), and Rust's `char::is_whitespace` is the Unicode
 * `White_Space` property. JavaScript's `String.prototype.trim` is not: its
 * `WhiteSpace` production is the `Zs` category plus a fixed list, and
 * **`U+0085 NEXT LINE` is in neither**. A string of nothing but `U+0085` is
 * therefore non-blank to JS and blank to the kernel — and a caller that gated
 * on `trim()` would enable the send, post that string, and collect a 400
 * nobody asked for.
 *
 * `\p{White_Space}` (with the `u` flag) *is* that Unicode property, so this
 * predicate and the kernel's agree by construction rather than by a hand-kept
 * code-point table, which would be wrong the next time Unicode adds one.
 *
 * It answers only "is this blank". It never returns a *value*: the text the
 * kernel stores, hashes (`first_message_digest` is verbatim, no trim) and
 * forwards to the agent is the reader's own string, indentation and trailing
 * spaces included, so trimming on the way to the wire would silently rewrite
 * what they typed.
 */
export function isBlankForKernel(text: string): boolean {
  return /^\p{White_Space}*$/u.test(text);
}

export type NewTrackBody = Readonly<{
  area_id: string;
  /**
   * Issue #1211 — optional. The title is no longer the track's intent: omit it
   * and the kernel stores the **empty string** — it has no default name of its
   * own; `UNTITLED_TRACK_LABEL` below is this layer's display fallback for a
   * blank title. What this field decides is only what THIS request stores;
   * who names the track afterwards is not its business — the planner agent via
   * `calm.track.rename` is the usual namer, but the user can name it first.
   * Present values (including `""`) are accepted verbatim.
   */
  title?: string;
  /**
   * Issue #1131 / #1147 — optional. The new FE omits it for a managed
   * workspace, which the server allocates beneath its workspace root. An Area
   * default or an explicit folder supplies the attached path instead; present
   * values keep the absolute-path + claim rules. `null` matches OpenAPI
   * (`string | null`) and is the same omitted branch.
   */
  cwd?: string | null;
  theme: ThemeRgb;
  /**
   * `false` requires `cwd` to already sit under a folder claimed by some area;
   * the route answers 409 `conflict` naming the area to claim it for. `true`
   * claims it in the same transaction. Omitting `cwd` forces this to `false`
   * on the kernel regardless of the field. When `cwd` is present, omitting
   * `attach_folder` is `false`.
   */
  attach_folder?: boolean;
  /**
   * The chosen template's key (#1209). Read as `template.id` from
   * `GET /api/track-templates`; the write side still spells it `template_id`
   * because on this field that name is accurate — it is what the kernel's
   * plugin-binding path resolves. #1209 records the seam and the decision not
   * to add a `template_id` alias.
   *
   * **Blank omits the key entirely.** Not `null`, not `''`: the kernel rejects
   * a whitespace-only id with a 400 and the body is
   * `deny_unknown_fields`-strict, so the only spelling of "no template" is
   * absence.
   */
  template_id?: string;
  /**
   * Only accepted when the chosen template is bound to a running trusted
   * plugin — i.e. exactly when `GET /api/track-templates` returned an
   * `input_schema` for it. Sending it otherwise is a 400.
   */
  template_input?: Readonly<Record<string, unknown>>;
  /**
   * The chosen user recipe's id (#1292) — a `track_recipes` row, read as
   * `recipe.id` from `GET /api/track-recipes`.
   *
   * **Mutually exclusive with `template_id` — and, since #1321 S2, with the
   * kernel's third starting point `fork_report_from` as well. Naming any two
   * is a 400 that names both**:
   *
   * ```
   * track create: `template_id` and `recipe_id` each name a starting point
   * for the new track's report; give at most one
   * ```
   *
   * (`NamedSource::from_request` in `routes/tracks.rs`.) *At most* one: naming
   * none is the ordinary blank create this type sends by default. Before
   * #1321 S2 only this one pair was refused — `fork_report_from` silently
   * outranked whichever other field was sent — and the quote here was the
   * older `"give `template_id` or `recipe_id`, not both"`, a string the kernel
   * no longer produces.
   *
   * `fork_report_from` is not a field of this type: no frontend creates a
   * track by forking, so the exclusivity above is stated for the caller
   * reading the wire contract, not for a shape this type can build.
   *
   * They are not two spellings of one field: a
   * `template_id` lands on `tracks.template_id`, which the start path later
   * resolves against running plugins' manifests, and a recipe id has no
   * manifest to resolve against — putting one there would make every
   * recipe-created track log a resolution failure for an entirely normal
   * situation. That exclusivity is why the picker's selection is a tagged
   * union rather than a bare id string: two id spaces in one `string` cannot
   * say which endpoint the value came from.
   *
   * Absent, never `''`: same rule as `template_id`.
   */
  recipe_id?: string;
  /**
   * Issue #1299 — the sentence the reader typed on the new-track page,
   * delivered to the track's planner agent **by this create** instead of
   * having to be retyped after landing on the track.
   *
   * The kernel seeds it as an `Observation::UserMessage` inside the same
   * `planner-harness-start` transaction, so it is delivered exactly once and
   * attributed to the human. It is **not** the track's `title` and not a
   * `TrackGoal`: those are different slots, and this one is "what the user
   * said first".
   *
   * **Blank omits the key entirely**, and blank is `isBlankForKernel` above —
   * the kernel's own criterion, not JS `trim()`. It validates this field
   * exactly like `POST /api/cards/{id}/planner/input` — non-blank after trim,
   * at most 32768 **characters** — and rejects the create with a 400 before
   * anything is minted, so posting a string it reads as blank would turn an
   * ordinary empty composer into a failed create. Absence is the only spelling
   * of "no first message" this layer uses.
   *
   * A value that *is* sent goes **verbatim**: the kernel forwards it to the
   * agent untrimmed and hashes it untrimmed, so whatever whitespace the reader
   * typed around their sentence is part of what they said.
   *
   * Typed `string` rather than `string | null` even though OpenAPI says
   * `string | null`: `null` is the wire's *second* spelling of the same
   * omitted branch (`#[serde(default)] Option<String>`), and offering it here
   * would let a caller send a key that means exactly what sending no key
   * means.
   *
   * Supplying it also changes what a failed harness start means: without it
   * that failure is still a 201 (an inert planner agent is recoverable), with
   * it the create answers 500 because the delivery it promised may or may not
   * have happened.
   *
   * Since #1384 sending it also makes `Idempotency-Key` **required** — see
   * `createTrackOperation`. Under that key the create IS retryable: the retry
   * lands on the track the first attempt already made and delivers no second
   * copy. What it still does not tell you is whether the first attempt's
   * delivery happened; the server cannot know that.
   */
  first_message?: string;
}>;

/**
 * A selectable starting point for a new track (#1209).
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
export const trackTemplateSchema = z.object({
  id: z.string(),
  title: z.string(),
  input_schema: z.unknown().optional(),
  /**
   * The tasks the template pre-sets, in plan order — the same `task` blocks
   * the created track's report is seeded with.
   *
   * Required, not `.default([])`: the server sends it for every template, and
   * a default would let a genuinely broken read render as "this template
   * pre-sets nothing", which is a lie the user cannot tell from the truth.
   */
  tasks: z.array(z.object({ key: z.string(), goal: z.string() })),
});
export type TrackTemplate = z.infer<typeof trackTemplateSchema>;

export function trackTemplatesOperation(): ApiOperation<TrackTemplate[]> {
  return { method: 'GET', path: '/api/track-templates', responseSchema: z.array(trackTemplateSchema) };
}

/**
 * A user-defined starting point for a new track (#1292) — a `track_recipes`
 * row.
 *
 * ## Why this is not `TrackTemplate` with a flag
 *
 * There is no combined endpoint and **no `builtin` / `read_only`
 * discriminator on either payload**; the kernel states that as intentional
 * (`routes/track_recipes.rs`): built-in and mine "differ only in where the
 * payload came from", and where it came from is *which endpoint answered*.
 * So the kind is tagged on merge, in the one place that merges them
 * (`features/area/new-track`), and neither wire type grows a field the server
 * does not have.
 *
 * The two are also shaped differently in the way that matters to a reader.
 * A template exposes `tasks[]` — structured, renderable as a hover card, and
 * never a `body`. A recipe exposes `body` — the Markdown whose `neige-block`
 * fences *are* its tasks — and no `tasks[]`. That asymmetry is why "duplicate
 * this built-in as my recipe" is not offered: producing a recipe body from a
 * template would mean re-implementing the kernel's `render_fence` in
 * TypeScript, i.e. a second fence writer, which is the duplication #1300 spent
 * a slice removing.
 */
export const trackRecipeSchema = z.object({
  id: z.string(),
  /** Picker label *and* the instantiated report's summary — one field on the
   *  kernel side too, so the editor edits one thing. */
  title: z.string(),
  /** The report body. Its `neige-block` fences are the recipe's tasks. */
  body: z.string(),
  /**
   * The optimistic-lock anchor a `PUT` must echo as `if_revision`. Not
   * `updated_at`: a wall clock is not a version.
   */
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
 * Whole-document replace, gated on the `revision` the caller read.
 *
 * Whole-body and not per-block CAS, deliberately: a recipe's only writer is
 * its owner, possibly from two windows, and the correct answer to a stale
 * write there is showing the second writer a conflict — not a merge engine.
 * A track's report needs block-level CAS because three parties write it
 * concurrently; a recipe has no such third party, and there is no partially
 * synced state in between.
 *
 * The response is the *stored* row, which is not always the bytes sent: the
 * write boundary re-renders every fence, drops tombstones and normalizes the
 * task privilege fields. Callers must render what comes back.
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
 * `POST /api/tracks`.
 *
 * `idempotencyKey` is **required by the kernel whenever `body.first_message` is
 * present** (#1384) and ignored entirely otherwise, which is why it is a
 * separate optional parameter rather than a body field: it travels as a header,
 * and an operation that cannot express a header cannot call this endpoint with
 * a first message at all.
 *
 * What the key buys, and it is the reason it must be minted **per draft and not
 * per call**: the kernel binds it to the track it creates, inside the same
 * transaction that mints the track id. A retry under the same key returns that
 * track and does not deliver the sentence a second time. A key minted per call
 * is a different key on the retry, and a different key mints a second track
 * holding the same message — the exact failure the header exists to stop.
 *
 * Not sent when `first_message` is absent: the kernel does not read it there,
 * and a message-less create is deliberately still not idempotent.
 */
export function createTrackOperation(
  body: NewTrackBody,
  idempotencyKey?: string,
): ApiOperation<TrackWire> {
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

/**
 * `POST /api/tracks/:id/codex-cards` — the atomic codex spawn. `theme` is
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

/**
 * `POST /api/tracks/:id/cards` — the kernel's direct-create path: the row is
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

export function createCardOperation(trackId: string, body: NewCardBody): ApiOperation<CardWire> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/cards`,
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

/**
 * #254 — the UI grouping predicate for every "Waiting on you" surface. ORs the
 * lifecycle bucket with the kernel `card_fsm`-derived overlay so a track whose
 * worker card is sitting on AwaitingInput surfaces even before the Planner Agent
 * has driven `working → blocked`.
 *
 * This stays separate from `isWaitingForUser` on purpose: the two signals have
 * different owners (Planner Agent vs kernel) and different storage (column vs
 * overlay), and places that genuinely want the pure lifecycle bucket — the
 * lifecycle badge, area bucket sort — must keep getting it.
 */
export function needsUserAttention(track: Track): boolean {
  return isWaitingForUser(track.lifecycle) || track.anyCardNeedsInput;
}

/** Waiting first, then running, then everything quiet. */
export function lifecycleRank(track: Track): number {
  if (needsUserAttention(track)) return 0;
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

/**
 * The tracks a person may see: not archived, and hosted by an area they may see.
 *
 * This is **not** a fix for a live leak — `areaListQueryOptions` already applies
 * `visibleAreas` in the query layer, and the workspace only fans out over what
 * that returned. It is the *second* layer of defence `visibleAreas` announces
 * (E2E-INV-SHELL-003), and it existed on exactly one of the two list surfaces:
 * the sidebar intersected areas and tracks by hand while mobile Pages filtered
 * tracks alone. One function, used by both, is what makes the stated intent true
 * at the component boundary rather than only in the query that happens to feed
 * it today (#1191 §3.1).
 */
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

/** #409 — one display fallback for tracks created without a title. */
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
 * #250 PR 5 — every track whose `[createdAt, terminalAt ?? nowMs]` interval
 * overlaps the local day owning `day`.
 *
 * Endpoints are inclusive (`createdAt <= endOfDay AND end >= startOfDay`) so a
 * track created at 23:59 still surfaces on that day even if its first card
 * lands a millisecond later. Sorted by `createdAt`, ties broken by id, so dot
 * ordering matches creation order (oldest leftmost — how the eye scans).
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
