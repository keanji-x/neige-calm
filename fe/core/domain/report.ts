// Reading a track's report out of its cards: a sequence of typed blocks carried in the
// `track-report` card's payload. `blocks[]` is authoritative (it carries the ids every
// deep link addresses); `body` is the flat fallback. Everything here is fail-soft.

import { z } from 'zod';

import type { CurrentTaskExecution } from './task-execution.js';
import type { ApiOperation } from '../api/types.js';
import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy,
} from '../markdown/public.js';
import type { CardWire } from './track.js';

/** The card kind the kernel reserves for the report. One per track, undeletable. */
export const TRACK_REPORT_CARD_KIND = 'track-report';

/* Per-kind block payloads. The bounds mirror the kernel's own validators, so a payload the
   kernel accepts is exactly a payload this renders. */

export const proseBlockPayloadSchema = z.object({ markdown: z.string() });

function max2048CodePoints(schema: z.ZodString) {
  return schema.refine((value) => [...value].length <= 2048, { message: 'String must contain at most 2048 character(s)' });
}

/** One candle: `[ts_ms, open, high, low, close, volume?]`. */
export const candleTupleSchema = z.tuple([
  z.number(), z.number(), z.number(), z.number(), z.number(), z.number().nullish(),
]);

export const chartCandlesPayloadSchema = z.strictObject({
  symbol: max2048CodePoints(z.string().min(1)),
  period: z.enum(['day', 'week', 'month']).nullish(),
  /** Data is inlined; range switching is a pure client-side filter. */
  candles: z.array(candleTupleSchema).min(2).max(5000),
  overlays: z.array(z.enum(['ma20', 'ma60'])).nullish(),
  caption: max2048CodePoints(z.string()).nullish(),
});

/**
 * A live `table` names where its rows come from: the overlay written by `<plugin_id>` under
 * `<overlay_kind>`. A shape check only — the plugin need not be installed.
 */
export const LIVE_TABLE_SOURCE_PATTERN = /^neige:\/\/plugin\/[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/;

/** The rows-carrying form — and the shape a live table's overlay must hold. */
export const inlineTableBlockPayloadSchema = z.strictObject({
  columns: z.array(z.strictObject({
    key: max2048CodePoints(z.string().min(1)),
    label: max2048CodePoints(z.string()),
    align: z.enum(['left', 'right']).nullish(),
  })).min(1).max(32),
  rows: z.array(z.record(
    z.string(),
    z.union([max2048CodePoints(z.string()), z.number(), z.null()]),
  )).max(500),
  caption: max2048CodePoints(z.string()).nullish(),
  highlight: max2048CodePoints(z.string()).nullish(),
})
  .refine((table) => new Set(table.columns.map((column) => column.key)).size === table.columns.length,
    { message: 'column keys must be unique' })
  .refine((table) => {
    const keys = new Set(table.columns.map((column) => column.key));
    return table.rows.every((row) => Object.keys(row).every((key) => keys.has(key)));
  }, { message: 'row keys must be declared column keys' });

export const liveTableBlockPayloadSchema = z.strictObject({
  source: max2048CodePoints(z.string().regex(LIVE_TABLE_SOURCE_PATTERN,
    'must be neige://plugin/<plugin_id>/<overlay_kind>')),
  caption: max2048CodePoints(z.string()).nullish(),
});

/** A first-class, read-only presentation reference, not an executable App. */
export const liveViewBlockPayloadSchema = z.strictObject({
  source: max2048CodePoints(z.string().regex(LIVE_TABLE_SOURCE_PATTERN)),
  version: z.literal(1),
  view: z.enum(['overview', 'activity', 'cards', 'details']),
});
export type LiveViewBlockPayload = z.infer<typeof liveViewBlockPayloadSchema>;

/* chart.series: a chart that names its data instead of carrying it. Like the kernel, `as_of`
   is never compared with today — a cutoff in the future is a frozen block the renderer must draw. */

/** `^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$` — venue prefix and symbol. */
export const CHART_SERIES_ASSET_PATTERN = /^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$/;

function isLeapYear(year: number): boolean {
  return (year % 4 === 0 && year % 100 !== 0) || year % 400 === 0;
}

function daysInMonth(year: number, month: number): number | null {
  switch (month) {
    case 1: case 3: case 5: case 7: case 8: case 10: case 12: return 31;
    case 4: case 6: case 9: case 11: return 30;
    case 2: return isLeapYear(year) ? 29 : 28;
    default: return null;
  }
}

/** `YYYY-MM-DD` that names a real Gregorian day. No `Date`: `new Date('2026-02-30')` rolls over to March. */
export function isCalendarDate(text: string): boolean {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (match === null) return false;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const last = daysInMonth(year, month);
  return last !== null && day >= 1 && day <= last;
}

export const chartSeriesPayloadSchema = z.strictObject({
  source: max2048CodePoints(z.string().regex(LIVE_TABLE_SOURCE_PATTERN,
    'must be neige://plugin/<plugin_id>/<tool>')),
  series: z.array(max2048CodePoints(z.string().regex(CHART_SERIES_ASSET_PATTERN,
    'must be a venue-qualified asset id such as US:NVDA'))).min(1).max(8),
  field: z.enum(['close', 'open', 'high', 'low', 'volume']).nullish(),
  range: z.enum(['1M', '3M', '6M', '1Y', '2Y', '5Y']).nullish(),
  period: z.enum(['day', 'week', 'month']).nullish(),
  view: z.enum(['line', 'normalized', 'bar', 'candles']).nullish(),
  as_of: max2048CodePoints(z.string()).nullish(),
  overlays: z.array(z.enum(['ma20', 'ma60'])).nullish(),
  caption: max2048CodePoints(z.string()).nullish(),
})
  .refine((chart) => new Set(chart.series).size === chart.series.length,
    { message: 'series must not repeat an asset' })
  .refine((chart) => chart.view !== 'candles' || (chart.series.length === 1 && chart.field == null),
    { message: 'view candles takes exactly one series and no field' })
  .refine((chart) => chart.overlays == null || chart.view == null || chart.view === 'line' || chart.view === 'candles',
    { message: 'overlays apply only to view line|candles' })
  .refine((chart) => chart.as_of == null || isCalendarDate(chart.as_of),
    { message: 'as_of must be a calendar date in YYYY-MM-DD form' })
  .refine((chart) => !(chart.range === '1M' && chart.period === 'month'),
    { message: 'range 1M cannot hold two complete month periods' });

/* Live first: both members are strict, so order only picks which error a malformed payload reports. */
export const tableBlockPayloadSchema = z.union([
  liveTableBlockPayloadSchema,
  inlineTableBlockPayloadSchema,
]);

/**
 * `src` is a same-origin absolute path: a leading `/`, not `//`, and no backslashes (browsers
 * normalize `\` to `/` inside a URL). The renderer re-asserts the origin: two checks for the one
 * block that loads someone else's markup.
 */
export const appBlockPayloadSchema = z.strictObject({
  src: max2048CodePoints(z.string()
    .regex(/^\/(?!\/)[^\\]*$/, { message: 'src must be a same-origin absolute path' })
    .refine((value) => {
      for (let index = 0; index < value.length; index += 1) {
        const code = value.charCodeAt(index);
        if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return false;
      }
      return true;
    }, { message: 'src must not contain control characters' })),
  title: max2048CodePoints(z.string()).nullish(),
  height: z.number().min(120).max(2000).nullish(),
});

const taskGateStepSchema = z.strictObject({ name: z.string(), cmd: z.string() });

function liveTaskSharedShape() {
  return {
    acceptance: z.string().nullish(),
    gate: z.strictObject({
      cwd: z.string().nullish(),
      timeout_secs: z.number().int().nullish(),
      steps: z.array(taskGateStepSchema),
    }).nullish(),
    no_gate_reason: z.string().nullish(),
    depends_on: z.array(z.string()).nullish(),
    priority: z.number().int().nullish(),
    cwd: z.string().nullish(),
    context: z.unknown().nullish(),
    refs: z.array(z.string()).nullish(),
    ready: z.boolean(),
    declared_by: z.enum(['spec', 'user']),
    released_by_user: z.boolean().nullish(),
    spawn: z.enum(['in-wave', 'sub-wave']).nullish(),
    tombstone: z.null().nullish(),
  };
}

const agentTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  kind: z.enum(['codex', 'claude']),
  goal: z.string(),
  ...liveTaskSharedShape(),
});

const terminalTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  kind: z.literal('terminal'),
  command: z.string(),
  ...liveTaskSharedShape(),
});

/** Read-only compatibility for older report payloads; new writes must use `command` for terminal tasks. */
const legacyTerminalTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  kind: z.literal('terminal'),
  goal: z.string(),
  ...liveTaskSharedShape(),
}).transform(({ goal, ...payload }) => ({ ...payload, command: goal }));

const liveTaskBlockPayloadSchema = z.union([
  agentTaskBlockPayloadSchema,
  terminalTaskBlockPayloadSchema,
  legacyTerminalTaskBlockPayloadSchema,
]);

/** A withdrawn task keeps its key and both attributions; other reports may cite its block id. */
const tombstoneTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  tombstone: z.strictObject({ reason: z.string().nullish() }),
  declared_by: z.enum(['spec', 'user']),
  tombstoned_by: z.enum(['spec', 'user']),
});

export const taskBlockPayloadSchema = z.union([
  liveTaskBlockPayloadSchema,
  tombstoneTaskBlockPayloadSchema,
]);

export type ProseBlockPayload = z.infer<typeof proseBlockPayloadSchema>;
export type ChartCandlesPayload = z.infer<typeof chartCandlesPayloadSchema>;
export type ChartSeriesPayload = z.infer<typeof chartSeriesPayloadSchema>;
export type TableBlockPayload = z.infer<typeof tableBlockPayloadSchema>;
export type InlineTableBlockPayload = z.infer<typeof inlineTableBlockPayloadSchema>;
export type LiveTableBlockPayload = z.infer<typeof liveTableBlockPayloadSchema>;

/** Narrows a table payload to its live form. */
export function isLiveTablePayload(payload: TableBlockPayload): payload is LiveTableBlockPayload {
  return 'source' in payload;
}
export type AppBlockPayload = z.infer<typeof appBlockPayloadSchema>;
export type TaskBlockPayload = z.infer<typeof taskBlockPayloadSchema>;

/**
 * A block, discriminated by `kind`, with `unsupported` as the closed default. `rev` is carried
 * only on `chart.series`, whose data request is bound to the block revision; it is a query key,
 * never a rendered value.
 */
export type ReportBlock =
  | Readonly<{ id: string; kind: 'prose'; payload: ProseBlockPayload }>
  | Readonly<{ id: string; kind: 'chart.candles'; payload: ChartCandlesPayload }>
  | Readonly<{ id: string; kind: 'chart.series'; rev: number; payload: ChartSeriesPayload }>
  | Readonly<{ id: string; kind: 'table'; payload: TableBlockPayload }>
  | Readonly<{ id: string; kind: 'view.live'; payload: LiveViewBlockPayload }>
  | Readonly<{ id: string; kind: 'app'; payload: AppBlockPayload }>
  | Readonly<{ id: string; kind: 'task'; payload: TaskBlockPayload }>
  | Readonly<{ id: string; kind: 'unsupported'; declaredKind: string }>;

const blockWireSchema = z.object({
  id: z.string().min(1),
  kind: z.string(),
  /* The kernel always writes one; optional on the read because only `chart.series` consumes it. */
  rev: z.number().int().nonnegative().optional(),
  payload: z.unknown(),
});

/** As a function rather than a frozen object: a module-level map of schemas would be module runtime state. */
function payloadSchemaFor(kind: string): z.ZodType | null {
  switch (kind) {
    case 'prose': return proseBlockPayloadSchema;
    case 'chart.candles': return chartCandlesPayloadSchema;
    case 'chart.series': return chartSeriesPayloadSchema;
    case 'table': return tableBlockPayloadSchema;
    case 'view.live': return liveViewBlockPayloadSchema;
    case 'app': return appBlockPayloadSchema;
    case 'task': return taskBlockPayloadSchema;
    default: return null;
  }
}

/** One wire block → one renderable block, degrading to `unsupported` rather than throwing. */
function toReportBlock(wire: z.infer<typeof blockWireSchema>): ReportBlock {
  const schema = payloadSchemaFor(wire.kind);
  if (schema === null) return { id: wire.id, kind: 'unsupported', declaredKind: wire.kind };
  const parsed = schema.safeParse(wire.payload);
  if (!parsed.success) return { id: wire.id, kind: 'unsupported', declaredKind: wire.kind };
  if (wire.kind === 'chart.series') {
    // A series block without a revision cannot ask for its data, so it degrades like an unreadable payload.
    if (wire.rev === undefined) return { id: wire.id, kind: 'unsupported', declaredKind: wire.kind };
    return { id: wire.id, kind: 'chart.series', rev: wire.rev, payload: parsed.data as ChartSeriesPayload };
  }
  // The discriminant and its payload were validated together above; TypeScript cannot carry that pairing.
  return { id: wire.id, kind: wire.kind, payload: parsed.data } as ReportBlock;
}

/** A deliberately narrow read: `schemaVersion` and `docRev` stay unparsed so a newer payload stays readable. */
export const trackReportPayloadSchema = z.object({
  summary: z.string().default(''),
  body: z.string().default(''),
  blocks: z.unknown().nullish(),
});

export type TrackReport = Readonly<{
  summary: string;
  /** Markdown source — the flat projection, and the only content a v1 row has. */
  body: string;
  /** `null` on a v1 row: blocks were not persisted, so `body` is all there is. */
  blocks: readonly ReportBlock[] | null;
}>;

/**
 * The track's report, or `null` when it has none — no card, an unparseable payload, or one empty in
 * both projections.
 */
export function readTrackReport(cards: readonly CardWire[]): TrackReport | null {
  const card = cards.find((candidate) => candidate.kind === TRACK_REPORT_CARD_KIND);
  if (card === undefined) return null;
  const parsed = trackReportPayloadSchema.safeParse(card.payload);
  if (!parsed.success) return null;
  const summary = parsed.data.summary.trim();
  const body = parsed.data.body.trim();
  // One malformed block must cost only that block.
  const blocks = Array.isArray(parsed.data.blocks)
    ? parsed.data.blocks.flatMap((candidate) => {
      const wire = blockWireSchema.safeParse(candidate);
      return wire.success ? [toReportBlock(wire.data)] : [];
    })
    : null;
  // An empty blocks array is the same emptiness as a blank body.
  if (summary === '' && body === '' && (blocks === null || blocks.length === 0)) return null;
  return { summary, body, blocks };
}

/* Outline: the shape is decided by the blocks, not by the headings. */

export type ReportOutlineChild = Readonly<{ blockId: string; label: string }>;
export type ReportOutlineItem = Readonly<{
  /** The anchor to scroll to: a heading id inside a prose block, or a block id. */
  blockId: string;
  label: string;
  /** Continuous across blocks; `null` for a non-prose block with no section above it. */
  number: number | null;
  children: readonly ReportOutlineChild[];
}>;

/** A non-prose block's label is the first payload field that names the thing it draws; the kind is the last resort. */
function blockLabel(block: ReportBlock): string {
  if (block.kind === 'unsupported') return block.declaredKind;
  if (block.kind === 'task') return block.payload.key;
  const payload: Record<string, unknown> = block.payload;
  for (const key of ['symbol', 'src', 'caption', 'title']) {
    const value = payload[key];
    if (typeof value === 'string' && value !== '') return value;
  }
  return block.kind;
}

/**
 * Derive the outline: every H1 is a numbered top-level item, an H2 hangs beneath the H1 above
 * it (or stays top-level with no preceding H1), and a non-prose block hangs under the numbered
 * section above it or becomes an unnumbered top-level item.
 */
export function deriveReportOutline(blocks: readonly ReportBlock[] | null): ReportOutlineItem[] {
  if (blocks === null) return [];
  const outline: ReportOutlineItem[] = [];
  let sectionNumber = 0;
  let lastNumbered: { children: ReportOutlineChild[] } | null = null;
  let lastH1: { children: ReportOutlineChild[] } | null = null;

  for (const block of blocks) {
    if (block.kind === 'prose') {
      const parsed = parse(block.payload.markdown);
      if (parsed.status === 'failed') continue;
      const headings = extractOutline([{ context: { blockId: block.id }, ast: parsed.value }], {
        maxDepth: REPORT_MAX_DEPTH,
        headingId: reportHeadingIdPolicy,
        textPolicy: 'non-empty-heading-label',
        referenceText: 'visible',
        traversal: 'recursive',
      });
      for (const heading of headings) {
        if (heading.depth === 2 && lastH1 !== null) {
          lastH1.children.push({ blockId: heading.id, label: heading.text });
          continue;
        }
        sectionNumber += 1;
        const children: ReportOutlineChild[] = [];
        outline.push({ blockId: heading.id, label: heading.text, number: sectionNumber, children });
        lastNumbered = { children };
        if (heading.depth === 1) lastH1 = { children };
      }
      continue;
    }
    // `task` blocks are lifted out of the flow into the `Reference` appendix, so they are not in the outline.
    if (isTaskBlock(block)) continue;
    const child: ReportOutlineChild = { blockId: block.id, label: blockLabel(block) };
    if (lastNumbered === null) {
      outline.push({ blockId: block.id, label: child.label, number: null, children: [] });
      continue;
    }
    lastNumbered.children.push(child);
  }
  return outline;
}

/* Tasks, as an inventory: derived from the declarations in the document, decorated with the
   kernel's run projection (`taskDiagnostics`) by `deriveReportTasks` below. */

/** A `task` block, including one this build could not read. One predicate, three readers. */
export function isTaskBlock(block: ReportBlock): boolean {
  return block.kind === 'task'
    || (block.kind === 'unsupported' && block.declaredKind === 'task');
}

/** `unreadable` is a task whose payload this build cannot parse: no key and no readiness, only an id. */
export type ReportTaskState = 'ready' | 'not-ready' | 'withdrawn' | 'unreadable';

const taskPendingReasonSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('dependencyBlocked'),
    message: z.string().min(1),
    dependencies: z.array(z.string()),
  }),
  z.object({
    kind: z.literal('budgetQueued'),
    message: z.string().min(1),
    occupiedTaskBudget: z.number().int().nonnegative(),
    effectiveTaskBudget: z.number().int().nonnegative(),
  }),
  z.object({
    kind: z.literal('notAdmitted'),
    message: z.string().min(1),
    diagnosticCodes: z.array(z.string()),
    actions: z.array(z.string()),
  }),
]);

export type TaskPendingReason = z.infer<typeof taskPendingReasonSchema>;

export const taskVerdictSchema = z.object({
  /** The declaring block; the same literal the report card's own `blocks[]` carries. */
  blockId: z.string(),
  key: z.string(),
  /** The projection's verdict on the *declaration*: ready, not withdrawn, and no blocking diagnostic. */
  schedulable: z.boolean(),
  /**
   * The `tasks` row's status; absent when the task has no row. Read as a string so an unknown
   * status is shown, not hidden.
   */
  status: z.string().nullish(),
  /** Why the row is in that status, in the kernel's own words; absent whenever the kernel has nothing to add. */
  statusDetail: z.string().nullish(),
  /** The worker card the task was dispatched onto; absent until claim. */
  workerCardId: z.string().nullish(),
  /** The server's finished answer to "why has this task not started?"; displayed verbatim, never re-derived. */
  pendingReason: taskPendingReasonSchema.nullish(),
});

export type TaskVerdict = z.infer<typeof taskVerdictSchema>;

/** Only `taskDiagnostics` is read; the rest of the response duplicates the report card this page already holds. */
const trackReportReadSchema = z.object({ taskDiagnostics: z.array(z.unknown()).default([]) })
  .transform((response) => response.taskDiagnostics.flatMap((candidate) => {
    // One malformed verdict costs only that row's runtime word.
    const parsed = taskVerdictSchema.safeParse(candidate);
    return parsed.success ? [parsed.data] : [];
  }));

/**
 * The `tasks` statuses inside the eventless window: `scheduler::mark_running` flips
 * `dispatched → running` and stamps `worker_card_id` with no event, so only those two need a
 * timer. `pending` and `verifying` are evented on every exit. An allowlist so an unknown
 * status degrades toward silence.
 */
function eventlessWindowTaskStatuses(): ReadonlySet<string> {
  return new Set(['dispatched', 'running']);
}

/**
 * Does any row this panel draws describe a run still inside the eventless window? Reads rows,
 * not verdicts: the kernel synthesises a verdict for a deleted declaration that no row shows.
 */
export function hasLiveTaskRun(rows: readonly ReportTaskRow[] | undefined): boolean {
  if (rows === undefined) return false;
  const live = eventlessWindowTaskStatuses();
  return rows.some((row) => row.status !== null && live.has(row.status));
}

/** The track's task verdicts; the route also answers with the report, which this discards. */
export function trackTaskVerdictsOperation(trackId: string): ApiOperation<TaskVerdict[]> {
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/report`,
    responseSchema: trackReportReadSchema,
  };
}

/** The three worker kinds a live task declaration can name; a verdict does not carry one. */
export type TaskWorkerKind = 'codex' | 'claude' | 'terminal';

/**
 * One row of the panel's task inventory. The declaration word, status and kind travel separately
 * because they are rendered separately.
 */
export type ReportTaskRow = Readonly<{
  /** Newer attempt/receipt evidence, when loaded by app; never copied into report diagnostics. */
  execution?: CurrentTaskExecution;
  /** The block to reveal in the document — the detail lives there, not here. */
  blockId: string;
  key: string;
  state: ReportTaskState;
  /** The word the *declaration* carries; `null` for the ordinary case and for a row that has a `status`. */
  declaration: string | null;
  /** The kernel's `tasks` row status, or `null` when there is no run this row may report. */
  status: string | null;
  /**
   * The kernel's reason for that status, only ever set alongside a `status`; already bounded to
   * `TASK_STATUS_DETAIL_LIMIT`.
   */
  statusDetail: string | null;
  /** The worker kind, off the *declaration*; a withdrawn or unreadable block has none. */
  kind: TaskWorkerKind | null;
  /** The worker card this task is running on, or `null`. */
  workerCardId: string | null;
  /** A server-rendered, actionable pending/admission explanation. */
  pendingReason: TaskPendingReason | null;
}>;

/** The detail's only destinations are an accessible name and a `title`, neither of which a stylesheet can truncate. */
export const TASK_STATUS_DETAIL_LIMIT = 160;

/** The kernel's reason as one bounded line, or `null`; whitespace is collapsed before the bound. */
export function boundedStatusDetail(raw: string | null | undefined): string | null {
  if (typeof raw !== 'string') return null;
  const collapsed = raw.replace(/\s+/g, ' ').trim();
  if (collapsed === '') return null;
  if (collapsed.length <= TASK_STATUS_DETAIL_LIMIT) return collapsed;
  /* The bound counts UTF-16 code units; a trailing lone high surrogate is dropped so the cut never
     lands inside a character. */
  const cut = collapsed.slice(0, TASK_STATUS_DETAIL_LIMIT - 1);
  const head = /[\uD800-\uDBFF]$/.test(cut) ? cut.slice(0, -1) : cut;
  return `${head.trimEnd()}…`;
}

/** The word, from the declaration alone. `ready` is silent on purpose. */
function declarationWord(state: ReportTaskState): string | null {
  if (state === 'ready') return null;
  if (state === 'withdrawn') return 'Withdrawn';
  if (state === 'unreadable') return 'Unreadable';
  return 'Not ready';
}

/**
 * Index the verdicts by block id (identity) and by key (fallback for drifted or deleted
 * declarations). A verdict naming a block this report has is kept out of the key index, and a
 * key more than one row claims is not a usable fallback for any of them.
 */
function indexVerdicts(
  verdicts: readonly TaskVerdict[],
  rowBlockIds: ReadonlySet<string>,
  keysClaimedByManyRows: ReadonlySet<string>,
) {
  const byBlockId = new Map<string, TaskVerdict>();
  const byKey = new Map<string, TaskVerdict>();
  for (const verdict of verdicts) {
    if (verdict.blockId !== '' && !byBlockId.has(verdict.blockId)) byBlockId.set(verdict.blockId, verdict);
    const namesARow = verdict.blockId !== '' && rowBlockIds.has(verdict.blockId);
    const ambiguousRow = keysClaimedByManyRows.has(verdict.key);
    if (verdict.key !== '' && !namesARow && !ambiguousRow && !byKey.has(verdict.key)) {
      byKey.set(verdict.key, verdict);
    }
  }
  return { byBlockId, byKey };
}

/**
 * The verdict that is about *this* block, or none. A block-id hit that contradicts the declared
 * key ends the lookup without falling back to the key index. A miss renders blank on purpose.
 */
function verdictFor(
  blockId: string, declaredKey: string,
  index: ReturnType<typeof indexVerdicts>,
): TaskVerdict | undefined {
  const byId = index.byBlockId.get(blockId);
  if (byId !== undefined) {
    return declaredKey === '' || byId.key === '' || byId.key === declaredKey ? byId : undefined;
  }
  return declaredKey === '' ? undefined : index.byKey.get(declaredKey);
}

/**
 * The row's state, from the declaration alone. `tombstoned_by` is the discriminant: a live task may
 * carry `tombstone: null`.
 */
function taskRowState(block: ReportBlock): ReportTaskState {
  if (block.kind !== 'task') return 'unreadable';
  if ('tombstoned_by' in block.payload) return 'withdrawn';
  return block.payload.ready ? 'ready' : 'not-ready';
}

/**
 * The key the block itself declared, or `''` for one this build cannot read; only a declared key
 * may look a verdict up.
 */
function declaredTaskKey(block: ReportBlock): string {
  return block.kind === 'task' ? block.payload.key : '';
}

/**
 * The keys two or more rows of this render declare, tombstones included; purely syntactic so it
 * cannot drift from the kernel.
 */
function keysDeclaredByMoreThanOneRow(blocks: readonly ReportBlock[]): ReadonlySet<string> {
  const seen = new Set<string>();
  const many = new Set<string>();
  for (const block of blocks) {
    if (!isTaskBlock(block)) continue;
    const key = declaredTaskKey(block);
    if (key === '') continue;
    if (seen.has(key)) many.add(key);
    else seen.add(key);
  }
  return many;
}

/**
 * Every `task` block, in document order, as one row each, decorated with the kernel's projection.
 * Withdrawn tasks are kept.
 */
export function deriveReportTasks(
  blocks: readonly ReportBlock[] | null,
  verdicts: readonly TaskVerdict[] = [],
): ReportTaskRow[] {
  if (blocks === null) return [];
  const rowBlockIds = new Set(blocks.filter(isTaskBlock).map((block) => block.id));
  const keysClaimedByManyRows = keysDeclaredByMoreThanOneRow(blocks);
  const index = indexVerdicts(verdicts, rowBlockIds, keysClaimedByManyRows);
  const rows: ReportTaskRow[] = [];
  for (const block of blocks) {
    if (!isTaskBlock(block)) continue;
    // An unreadable task still gets a row, and its id stands in for the name.
    const isReadable = block.kind === 'task';
    const state = taskRowState(block);
    const declaredKey = declaredTaskKey(block);
    /* `kind` lives on the live declaration, not on the verdict. */
    const kind: TaskWorkerKind | null = isReadable && !('tombstoned_by' in block.payload)
      ? block.payload.kind
      : null;
    /* `key` may be empty on the wire; the block id stands in so the row always has a name. */
    const key = declaredKey === '' ? block.id : declaredKey;
    /* A withdrawn or unreadable declaration takes no runtime decoration: withdrawal does not delete
       the `tasks` row, so its verdict keeps reporting a status. */
    const decorated = state !== 'withdrawn' && state !== 'unreadable';
    const verdict = decorated ? verdictFor(block.id, declaredKey, index) : undefined;
    /* `''` is not a status: the wire types it as an optional string, and an empty one would pass every `null` gate. */
    const status = verdict?.status === undefined || verdict.status === null || verdict.status === ''
      ? null
      : verdict.status;
    const pendingReason = decorated && verdict?.pendingReason !== undefined
      ? verdict.pendingReason ?? null
      : null;
    rows.push({
      blockId: block.id,
      key,
      state,
      /* No `tasks` row means nothing has run; the declaration word stands down once there is a status. */
      declaration: status === null ? declarationWord(state) : null,
      status,
      /* Gated on `status`: the detail is a qualifier on the status word and has nowhere to attach without one. */
      statusDetail: status === null ? null : boundedStatusDetail(verdict?.statusDetail),
      kind,
      /* `''` is not a card id. */
      workerCardId: verdict?.workerCardId === undefined || verdict.workerCardId === null
        || verdict.workerCardId === '' ? null : verdict.workerCardId,
      pendingReason,
    });
  }
  return rows;
}

/* Backlinks: the kernel resolves `neige://wave/<id>#<block>` links in other tracks' reports. */

export const backlinkQuoteSchema = z.object({
  before: z.string(),
  label: z.string(),
  after: z.string(),
  head_elided: z.boolean(),
  tail_elided: z.boolean(),
});

export const trackBacklinkSchema = z.object({
  src_track_id: z.string(),
  src_track_title: z.string(),
  src_block_id: z.string(),
  dst_block_id: z.string().nullish(),
  label: z.string(),
  quote: backlinkQuoteSchema.nullish(),
  updated_at: z.number(),
});

/** `truncated` and `skipped_sources` are read, not dropped: a knowingly incomplete list must say so. */
export const trackBacklinksSchema = z.object({
  backlinks: z.array(trackBacklinkSchema),
  truncated: z.boolean().default(false),
  skipped_sources: z.number().default(0),
});

export type BacklinkQuote = z.infer<typeof backlinkQuoteSchema>;
export type TrackBacklink = z.infer<typeof trackBacklinkSchema>;
export type TrackBacklinks = z.infer<typeof trackBacklinksSchema>;

export function trackBacklinksOperation(trackId: string): ApiOperation<TrackBacklinks> {
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/backlinks`,
    responseSchema: trackBacklinksSchema,
  };
}

/** Group by source track, preserving server order. */
export function groupBacklinks(
  backlinks: readonly TrackBacklink[],
  currentTrackId: string,
): readonly Readonly<{ trackId: string; title: string; entries: readonly TrackBacklink[] }>[] {
  const groups = new Map<string, { trackId: string; title: string; entries: TrackBacklink[] }>();
  for (const backlink of backlinks) {
    const group = groups.get(backlink.src_track_id);
    if (group !== undefined) {
      group.entries.push(backlink);
      continue;
    }
    groups.set(backlink.src_track_id, {
      trackId: backlink.src_track_id,
      title: backlink.src_track_id === currentTrackId
        ? 'This track (self-reference)'
        : backlink.src_track_title,
      entries: [backlink],
    });
  }
  return [...groups.values()];
}

/** How many backlinks land on each block, for the sidenote markers. */
export function backlinkCountsByBlock(
  backlinks: readonly TrackBacklink[],
): ReadonlyMap<string, number> {
  const counts = new Map<string, number>();
  for (const backlink of backlinks) {
    const target = backlink.dst_block_id;
    if (target === null || target === undefined || target === '') continue;
    counts.set(target, (counts.get(target) ?? 0) + 1);
  }
  return counts;
}

/* `neige://wave/<id>[#<block id>]` links, resolved here so the renderer never holds a URL. */

const NEIGE_WAVE_LINK = /^neige:\/\/wave\/([^/?#]+)(?:#([^#]+))?$/;

/** Block ids the kernel mints. A link whose fragment is not one of these keeps the track and drops the fragment. */
const BLOCK_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

export type ReportLinkTarget = Readonly<{ trackId: string; blockId: string | null }>;

export function parseReportLink(destination: string): ReportLinkTarget | null {
  const match = NEIGE_WAVE_LINK.exec(destination);
  if (match === null) return null;
  const trackId = match[1] ?? '';
  const blockId = match[2];
  if (trackId === '') return null;
  let decodedTrackId = trackId;
  try {
    decodedTrackId = decodeURIComponent(trackId);
  } catch {
    // Agent-written links must remain navigable even when an escape is malformed.
  }
  return {
    trackId: decodedTrackId,
    blockId: blockId !== undefined && BLOCK_ID_PATTERN.test(blockId) ? blockId : null,
  };
}
