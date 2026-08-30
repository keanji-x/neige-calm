// Reading a wave's report out of its cards.
//
// The report is not a thing this frontend invents: `WaveReportPayload` is a
// Tier-A persisted payload in the kernel (`crates/calm-types/src/wave_report.rs`,
// mirrored into `core/api/generated/wire.ts`), carried in the `payload` column
// of the wave's one `wave-report` card.
//
// A report is a **sequence of typed blocks**, not a Markdown string (§8.3).
// `body` is the kernel's lossless flat projection of those blocks, and it is
// what a v1 row carries; `blocks[]` is the authoritative layout since schema
// v2. Only `blocks[]` carries each block's stable **id**, and that id is the
// anchor every deep link, every backlink and the outline all address — which
// is why this module reads blocks and keeps `body` as the fallback rather than
// the other way round.
//
// Everything here is fail-soft. A card payload is `unknown` on the wire and a
// wave whose report has never been written carries `{}`; neither is an error,
// both are "no report yet". A block whose payload does not match its kind is
// kept as an opaque block so the renderer can degrade that one block — a
// report is written by an agent, so it will eventually contain something this
// build does not know about, and that must not cost the reader the page.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy,
} from '../markdown/public.js';
import type { CardWire } from './wave.js';

/** The card kind the kernel reserves for the report. One per wave, undeletable. */
export const WAVE_REPORT_CARD_KIND = 'wave-report';

/* ── Per-kind block payloads ────────────────────────────────────────────
   Each `kind` selects a typed payload. The bounds mirror the kernel's own
   validators (`crates/calm-truth`), so a payload the kernel accepts is
   exactly a payload this renders: a mismatch here would either reject a
   legal report or render one the kernel would have refused to store. */

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

export const tableBlockPayloadSchema = z.strictObject({
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

/**
 * `src` is a same-origin absolute path: a leading `/`, not the
 * protocol-relative `//`, and no backslashes — browsers normalize `\` to `/`
 * inside a URL, so a prefix check alone would let `/\host` through as `//host`.
 * C0/C1 control characters are refused for the same reason the Rust validator
 * refuses them. The renderer resolves the URL and re-asserts the origin: this
 * is the one block that loads someone else's markup, so it gets two checks.
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

const liveTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  kind: z.enum(['codex', 'claude', 'terminal']),
  goal: z.string(),
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
});

/** A withdrawn task keeps its key and both attributions: who declared it and
 *  who withdrew it. Dropping the row instead would make a task the report once
 *  carried simply vanish from a document people cite by block id. */
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
export type TableBlockPayload = z.infer<typeof tableBlockPayloadSchema>;
export type AppBlockPayload = z.infer<typeof appBlockPayloadSchema>;
export type TaskBlockPayload = z.infer<typeof taskBlockPayloadSchema>;

/**
 * A block, discriminated by `kind`, with `unsupported` as the closed default.
 *
 * `rev` is read but not exposed: it is the persistence layer's optimistic
 * concurrency counter, and §8.3 puts it on the list of things this surface
 * never shows.
 */
export type ReportBlock =
  | Readonly<{ id: string; kind: 'prose'; payload: ProseBlockPayload }>
  | Readonly<{ id: string; kind: 'chart.candles'; payload: ChartCandlesPayload }>
  | Readonly<{ id: string; kind: 'table'; payload: TableBlockPayload }>
  | Readonly<{ id: string; kind: 'app'; payload: AppBlockPayload }>
  | Readonly<{ id: string; kind: 'task'; payload: TaskBlockPayload }>
  | Readonly<{ id: string; kind: 'unsupported'; declaredKind: string }>;

const blockWireSchema = z.object({
  id: z.string().min(1),
  kind: z.string(),
  payload: z.unknown(),
});

/** The kind → payload-schema table, as a function rather than a frozen object:
 *  a module-level map of schemas would be module runtime state, and deep
 *  freezing a zod schema to satisfy that rule would reach inside zod. */
function payloadSchemaFor(kind: string): z.ZodType | null {
  switch (kind) {
    case 'prose': return proseBlockPayloadSchema;
    case 'chart.candles': return chartCandlesPayloadSchema;
    case 'table': return tableBlockPayloadSchema;
    case 'app': return appBlockPayloadSchema;
    case 'task': return taskBlockPayloadSchema;
    default: return null;
  }
}

/**
 * One wire block → one renderable block, degrading to `unsupported` rather
 * than throwing. Two different facts land in the same place on purpose: a kind
 * this build has never heard of, and a kind it knows whose payload it cannot
 * read. To a reader both are "this build cannot draw this", and the renderer
 * says exactly that in one line.
 */
function toReportBlock(wire: z.infer<typeof blockWireSchema>): ReportBlock {
  const schema = payloadSchemaFor(wire.kind);
  if (schema === null) return { id: wire.id, kind: 'unsupported', declaredKind: wire.kind };
  const parsed = schema.safeParse(wire.payload);
  if (!parsed.success) return { id: wire.id, kind: 'unsupported', declaredKind: wire.kind };
  // The discriminant and its payload were validated together one line above;
  // TypeScript cannot carry that pairing through an index into KIND_PAYLOADS.
  return { id: wire.id, kind: wire.kind, payload: parsed.data } as ReportBlock;
}

/**
 * A deliberately *narrow* read of `WaveReportPayload`.
 *
 * `schemaVersion` and `docRev` stay unparsed — the persistence layer's
 * business, and reading them would make a v4 payload unreadable to a viewer
 * that does not care what changed. Block ids and kinds are not version fields;
 * they are the content, which is why they are read.
 */
export const waveReportPayloadSchema = z.object({
  summary: z.string().default(''),
  body: z.string().default(''),
  blocks: z.unknown().nullish(),
});

export type WaveReport = Readonly<{
  summary: string;
  /** Markdown source — the flat projection, and the only content a v1 row has. */
  body: string;
  /** `null` on a v1 row: blocks were not persisted, so `body` is all there is. */
  blocks: readonly ReportBlock[] | null;
}>;

/**
 * The wave's report, or `null` when it has none.
 *
 * "None" covers three cases that are the same to a reader: no report card, a
 * payload that does not parse, and a payload that is empty in both projections.
 * A wave that has been created but never worked on is in the third case, which
 * is the common one — so it must not look like a failure.
 */
export function readWaveReport(cards: readonly CardWire[]): WaveReport | null {
  const card = cards.find((candidate) => candidate.kind === WAVE_REPORT_CARD_KIND);
  if (card === undefined) return null;
  const parsed = waveReportPayloadSchema.safeParse(card.payload);
  if (!parsed.success) return null;
  const summary = parsed.data.summary.trim();
  const body = parsed.data.body.trim();
  // One malformed block must cost only that block: `body` is still a complete
  // flat projection, and the other blocks still carry usable layout and ids.
  const blocks = Array.isArray(parsed.data.blocks)
    ? parsed.data.blocks.flatMap((candidate) => {
      const wire = blockWireSchema.safeParse(candidate);
      return wire.success ? [toReportBlock(wire.data)] : [];
    })
    : null;
  // A blocks array that exists but is empty is the same emptiness as a blank
  // body; it is not a document with zero sections that deserves a frame.
  if (summary === '' && body === '' && (blocks === null || blocks.length === 0)) return null;
  return { summary, body, blocks };
}

/* ── Outline ────────────────────────────────────────────────────────────
   The shape of the outline is decided by the blocks, not by the headings
   (§6.16). Three rules, one per projection the kernel can produce. */

export type ReportOutlineChild = Readonly<{ blockId: string; label: string }>;
export type ReportOutlineItem = Readonly<{
  /** The anchor to scroll to: a heading id inside a prose block, or a block id. */
  blockId: string;
  label: string;
  /** Continuous across blocks; `null` for a non-prose block with no section above it. */
  number: number | null;
  children: readonly ReportOutlineChild[];
}>;

/**
 * A non-prose block's label is the first payload field that names the thing it
 * draws. The kind is the last resort, not a label: an outline reading `table`,
 * `table`, `chart.candles` answers nothing.
 */
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
 * Derive the outline (§6.16):
 *  1. every H1/H2 inside a prose block is a numbered top-level item, numbered
 *     continuously across blocks — a reader counts sections of the report, not
 *     sections of a block they cannot see;
 *  2. a non-prose block hangs under the numbered section above it, as a child:
 *     it is evidence inside that section, not a section;
 *  3. with no numbered section above it, a non-prose block becomes an unnumbered
 *     top-level item — a report that opens with a table still has to be navigable.
 *
 * Those three cases are exhaustive, which is why there is no third level.
 */
export function deriveReportOutline(blocks: readonly ReportBlock[] | null): ReportOutlineItem[] {
  if (blocks === null) return [];
  const outline: ReportOutlineItem[] = [];
  let sectionNumber = 0;
  let lastNumbered: { children: ReportOutlineChild[] } | null = null;

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
        sectionNumber += 1;
        const children: ReportOutlineChild[] = [];
        outline.push({ blockId: heading.id, label: heading.text, number: sectionNumber, children });
        lastNumbered = { children };
      }
      continue;
    }
    /*
     * `task` blocks are not in the flow any more.
     *
     * `features/report/document` lifts every one of them out of the document
     * and into the collapsed `Reference` appendix at the end, so hanging them
     * under the prose section they used to follow would point the outline at a
     * place they are no longer drawn — and on a real wave that was eight rows
     * of machinery in a map of eight rows of argument.
     *
     * They stay reachable, by the two routes that are actually about tasks: the
     * panel's TASKS inventory, and any `neige://` link that cites one. Both go
     * through `revealReportAnchor`, which unfolds the appendix on the way.
     */
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

/* ── Tasks, as an inventory ─────────────────────────────────────────────

   A wave's tasks are report blocks (#229), which is the right place to *store*
   them and the wrong place to read them as a list: they sit wherever in the
   prose the agent happened to declare them, each one carrying the worker prompt
   and the gate commands that were written for a machine. Measured on a real
   wave: 8141 characters of report body, of which the prose a reader is meant to
   take away was ~700 and seven task blocks were the rest.

   So the panel gets the inventory and the document keeps the declarations. This
   derives the first from the second — no new endpoint, no second source of
   truth, and nothing that can disagree with what the document says.

   **What it cannot say, stated plainly: whether a task has run.** A block is a
   *declaration* — `ready` means the agent has finished writing it, not that the
   kernel has scheduled it, and there is no field here for pending / running /
   done / failed. The kernel does project that (`task_projection`), and
   `calm.plan.list` exposes it to MCP, but no HTTP route does. A status column
   is therefore a backend slice and not a display change; until it exists this
   list answers "what work has been declared", which is a different and smaller
   question than "how is it going". */

/**
 * A `task` block, **including one this build could not read**.
 *
 * A task payload that fails its schema degrades to `{ kind: 'unsupported',
 * declaredKind: 'task' }`, and failing to read it does not change what it is.
 * Three projections ask this question — the outline (which skips tasks), the
 * panel's inventory, and the document's `Reference` appendix — and the first
 * round of this change answered it three different ways: the appendix lifted an
 * unreadable task out of the flow while the outline still listed it and the
 * panel did not, so the one block nobody could read was also the one block
 * whose three views disagreed. Worse, the reason the outline is allowed to skip
 * tasks at all is that the panel still lists them; for this block that
 * justification was false.
 *
 * One predicate, three readers.
 */
export function isTaskBlock(block: ReportBlock): boolean {
  return block.kind === 'task'
    || (block.kind === 'unsupported' && block.declaredKind === 'task');
}

/** `unreadable` is a task whose payload this build cannot parse — see
 *  `isTaskBlock`. It has no key and no readiness, only an id. */
export type ReportTaskState = 'ready' | 'not-ready' | 'withdrawn' | 'unreadable';

export type ReportTaskRow = Readonly<{
  /** The block to reveal in the document — the detail lives there, not here. */
  blockId: string;
  key: string;
  state: ReportTaskState;
}>;

/**
 * Every `task` block, in document order, as one row each.
 *
 * Withdrawn tasks are kept, for the same reason the block itself keeps them:
 * the task existed, other reports may cite its block id, and a list that
 * silently dropped it would disagree with the document it is derived from.
 */
export function deriveReportTasks(blocks: readonly ReportBlock[] | null): ReportTaskRow[] {
  if (blocks === null) return [];
  const rows: ReportTaskRow[] = [];
  for (const block of blocks) {
    if (!isTaskBlock(block)) continue;
    /*
     * An unreadable task still gets a row, and its id stands in for the name.
     * Omitting it would leave one block that the outline skips (because tasks
     * live in the panel) and the panel does not list (because it has no key) —
     * reachable only by scrolling. The id is not a substitute for the key, but
     * it *is* the literal other reports cite this block by, which is the one
     * thing still true about it.
     */
    if (block.kind !== 'task') {
      rows.push({ blockId: block.id, key: block.id, state: 'unreadable' });
      continue;
    }
    const payload = block.payload;
    /* `tombstoned_by` is the discriminant, not `tombstone`: a live task may
       carry an explicit `tombstone: null`, so the key's presence proves
       nothing. Same test as `features/report/task`. */
    const state: ReportTaskState = 'tombstoned_by' in payload
      ? 'withdrawn'
      : payload.ready ? 'ready' : 'not-ready';
    /*
     * A row always has a name. `key` is `z.string()` — the kernel does not
     * require it to be non-empty — and an empty one reaches the panel as a
     * button with no text and therefore no accessible name: unreadable to a
     * screen reader, invisible to a pointer, and a `getByRole('button')` that
     * matches nothing. The same fallback the unreadable branch uses applies,
     * for the same reason: the block id is the literal other reports cite this
     * block by, which is the one name it always has.
     */
    rows.push({ blockId: block.id, key: payload.key === '' ? block.id : payload.key, state });
  }
  return rows;
}

/* ── Backlinks ──────────────────────────────────────────────────────────
   Who cites this wave. The kernel resolves `neige://wave/<id>#<block>` links
   found in other waves' reports and hands back a bounded page. */

export const backlinkQuoteSchema = z.object({
  before: z.string(),
  label: z.string(),
  after: z.string(),
  head_elided: z.boolean(),
  tail_elided: z.boolean(),
});

export const waveBacklinkSchema = z.object({
  src_wave_id: z.string(),
  src_wave_title: z.string(),
  src_block_id: z.string(),
  dst_block_id: z.string().nullish(),
  label: z.string(),
  quote: backlinkQuoteSchema.nullish(),
  updated_at: z.number(),
});

/** `truncated` and `skipped_sources` are read, not dropped: a backlink list
 *  that is knowingly incomplete must say so (§8.3) — silently short is the one
 *  failure mode a citation list may not have. */
export const waveBacklinksSchema = z.object({
  backlinks: z.array(waveBacklinkSchema),
  truncated: z.boolean().default(false),
  skipped_sources: z.number().default(0),
});

export type BacklinkQuote = z.infer<typeof backlinkQuoteSchema>;
export type WaveBacklink = z.infer<typeof waveBacklinkSchema>;
export type WaveBacklinks = z.infer<typeof waveBacklinksSchema>;

export function waveBacklinksOperation(waveId: string): ApiOperation<WaveBacklinks> {
  return {
    method: 'GET',
    path: `/api/waves/${encodeURIComponent(waveId)}/backlinks`,
    responseSchema: waveBacklinksSchema,
  };
}

/** Group by source wave, preserving server order — the panel prints one
 *  heading per citing wave, not one per citation. */
export function groupBacklinks(
  backlinks: readonly WaveBacklink[],
  currentWaveId: string,
): readonly Readonly<{ waveId: string; title: string; entries: readonly WaveBacklink[] }>[] {
  const groups = new Map<string, { waveId: string; title: string; entries: WaveBacklink[] }>();
  for (const backlink of backlinks) {
    const group = groups.get(backlink.src_wave_id);
    if (group !== undefined) {
      group.entries.push(backlink);
      continue;
    }
    groups.set(backlink.src_wave_id, {
      waveId: backlink.src_wave_id,
      title: backlink.src_wave_id === currentWaveId
        ? 'This wave (self-reference)'
        : backlink.src_wave_title,
      entries: [backlink],
    });
  }
  return [...groups.values()];
}

/** How many backlinks land on each block, for the sidenote markers (§8.3). */
export function backlinkCountsByBlock(
  backlinks: readonly WaveBacklink[],
): ReadonlyMap<string, number> {
  const counts = new Map<string, number>();
  for (const backlink of backlinks) {
    const target = backlink.dst_block_id;
    if (target === null || target === undefined || target === '') continue;
    counts.set(target, (counts.get(target) ?? 0) + 1);
  }
  return counts;
}

/* ── `neige://` links ───────────────────────────────────────────────────
   A report cites another wave with `neige://wave/<id>[#<block id>]`. The
   destination is resolved here, in core, so the renderer never has to hold a
   URL: it gets a wave id and an optional block id, or nothing. */

const NEIGE_WAVE_LINK = /^neige:\/\/wave\/([^/?#]+)(?:#([^#]+))?$/;

/** Block ids the kernel mints. A link whose fragment is not one of these keeps
 *  the wave and drops the fragment: landing at the top of the right report
 *  beats a dead link. */
const BLOCK_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

export type ReportLinkTarget = Readonly<{ waveId: string; blockId: string | null }>;

export function parseReportLink(destination: string): ReportLinkTarget | null {
  const match = NEIGE_WAVE_LINK.exec(destination);
  if (match === null) return null;
  const waveId = match[1] ?? '';
  const blockId = match[2];
  if (waveId === '') return null;
  let decodedWaveId = waveId;
  try {
    decodedWaveId = decodeURIComponent(waveId);
  } catch {
    // Agent-written links must remain navigable even when an escape is malformed.
  }
  return {
    waveId: decodedWaveId,
    blockId: blockId !== undefined && BLOCK_ID_PATTERN.test(blockId) ? blockId : null,
  };
}
