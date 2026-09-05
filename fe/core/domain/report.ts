// Reading a track's report out of its cards.
//
// The report is not a thing this frontend invents: `TrackReportPayload` is a
// Tier-A persisted payload in the kernel (`crates/calm-types/src/track_report.rs`,
// mirrored into `core/api/generated/wire.ts`), carried in the `payload` column
// of the track's one `track-report` card.
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
// track whose report has never been written carries `{}`; neither is an error,
// both are "no report yet". A block whose payload does not match its kind is
// kept as an opaque block so the renderer can degrade that one block — a
// report is written by an agent, so it will eventually contain something this
// build does not know about, and that must not cost the reader the page.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy,
} from '../markdown/public.js';
import type { CardWire } from './track.js';

/** The card kind the kernel reserves for the report. One per track, undeletable. */
export const TRACK_REPORT_CARD_KIND = 'track-report';

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

/** Read-only compatibility for report payloads persisted before #1456. New
 * writes are rejected by the kernel unless terminal tasks use `command`. */
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
 * A deliberately *narrow* read of `TrackReportPayload`.
 *
 * `schemaVersion` and `docRev` stay unparsed — the persistence layer's
 * business, and reading them would make a v4 payload unreadable to a viewer
 * that does not care what changed. Block ids and kinds are not version fields;
 * they are the content, which is why they are read.
 */
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
 * The track's report, or `null` when it has none.
 *
 * "None" covers three cases that are the same to a reader: no report card, a
 * payload that does not parse, and a payload that is empty in both projections.
 * A track that has been created but never worked on is in the third case, which
 * is the common one — so it must not look like a failure.
 */
export function readTrackReport(cards: readonly CardWire[]): TrackReport | null {
  const card = cards.find((candidate) => candidate.kind === TRACK_REPORT_CARD_KIND);
  if (card === undefined) return null;
  const parsed = trackReportPayloadSchema.safeParse(card.payload);
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
     * place they are no longer drawn — and on a real track that was eight rows
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

   A track's tasks are report blocks (#229), which is the right place to *store*
   them and the wrong place to read them as a list: they sit wherever in the
   prose the agent happened to declare them, each one carrying the worker prompt
   and the gate commands that were written for a machine. Measured on a real
   track: 8141 characters of report body, of which the prose a reader is meant to
   take away was ~700 and seven task blocks were the rest.

   So the panel gets the inventory and the document keeps the declarations. This
   derives the first from the second — no new endpoint, no second source of
   truth, and nothing that can disagree with what the document says.

   **What a block alone cannot say: whether a task has run.** A block is a
   *declaration* — `ready` means the agent has finished writing it, not that the
   kernel has scheduled it, and there is no field here for pending / running /
   done / failed. That was once the end of the story, and this list answered
   "what work has been declared", which is a different and smaller question than
   "how is it going": a user who dispatched four tasks could not tell which were
   running or which worker was on which.

   It is no longer. The kernel projects the run (`task_projection`) and `GET
   /api/tracks/{id}/report` exposes it as `taskDiagnostics`, so the second half
   arrives as a *decoration* on this list rather than as a second list — see
   `deriveReportTasks` below, which stays a pure join over the two. */

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

/* ── The runtime half ───────────────────────────────────────────────────

   The paragraph above says a block cannot say whether a task has run, and that
   a status column would be a backend slice. That slice exists: `GET
   /api/tracks/{id}/report` answers with `taskDiagnostics`, the kernel's own
   `BlockVerdict[]` — one entry per declared task, carrying `schedulable` from
   the projection and, when the `tasks` table has a row for that key, its
   `status` and the `workerCardId` the work was dispatched onto.

   Two sources, still one list: the declarations stay the spine (they are what
   the document draws and what the panel indexes), and a verdict only decorates
   a row that already exists. A verdict for a key no block declares is dropped
   — it would be a row nothing in the document backs. */

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
  /** The declaring block. The strongest join there is: it is the same literal
   *  the report card's own `blocks[]` carries. */
  blockId: z.string(),
  key: z.string(),
  /** The projection's verdict on the *declaration*: ready, not withdrawn, and
   *  no blocking diagnostic. It is a fact even before anything runs. */
  schedulable: z.boolean(),
  /**
   * The `tasks` row's status — `pending` / `running` / `verifying` / `done` /
   * `failed` / `canceled`. Absent (`skip_serializing_if`) when the task has no
   * row at all, which is exactly "declared but never dispatched", and is why
   * this is read as `string | null` rather than an enum: a status this build
   * has not heard of is printed as-is, which is more useful than hiding it.
   */
  status: z.string().nullish(),
  /**
   * Why the row is in that status, in the kernel's own words — the dispatch or
   * run failure that produced `failed` (#1147: `tasks.status_detail`, surfaced
   * on `BlockVerdict`). Absent whenever the kernel has nothing to add, which is
   * every ordinary status, so this is `nullish` for the same reason `status` is
   * and not because a missing one is a defect.
   *
   * Free-form prose, not an enum: it is the kernel's message verbatim (`track …
   * is not a git repository`), which is the entire value — `spawn-failed` was
   * already knowable from `failed`.
   */
  statusDetail: z.string().nullish(),
  /** The worker card the task was dispatched onto; absent until claim. */
  workerCardId: z.string().nullish(),
  /**
   * The server's finished answer to "why has this task not started?". The
   * frontend displays `message` verbatim and never combines `schedulable`,
   * dependency rows, budget defaults, or diagnostics into a second scheduler.
   */
  pendingReason: taskPendingReasonSchema.nullish(),
});

export type TaskVerdict = z.infer<typeof taskVerdictSchema>;

/** Only `taskDiagnostics` is read. The rest of the response duplicates the
 *  report card this page already holds, and reading it twice would give the
 *  document two sources that can disagree. */
const trackReportReadSchema = z.object({ taskDiagnostics: z.array(z.unknown()).default([]) })
  .transform((response) => response.taskDiagnostics.flatMap((candidate) => {
    // One malformed verdict costs only that row's runtime word, exactly as one
    // malformed block costs only that block.
    const parsed = taskVerdictSchema.safeParse(candidate);
    return parsed.success ? [parsed.data] : [];
  }));

/**
 * The `tasks` statuses that sit inside the **eventless window** — the only
 * thing a timer is here to close.
 *
 * `TaskStatus` (`calm-truth/src/model.rs`) has seven words: `Pending`,
 * `Dispatched`, `Running`, `Verifying`, `Done`, `Failed`, `Canceled`. This is
 * deliberately *not* "the four non-terminal ones". Being non-terminal is not
 * the question; being unobservable is. A status belongs here only if the write
 * that moves the row out of it emits no event this query listens to, because a
 * status whose every exit is evented already converges without a timer, and
 * polling it is pure cost — unbounded cost, since nothing bounds how long a row
 * may sit in one status.
 *
 * The window opens at `mark_running` (`scheduler/mod.rs`), the one write the
 * panel needs and the one write that is silent: it flips `dispatched → running`
 * and stamps `worker_card_id` in a plain guarded UPDATE with no event riding
 * along (its own comment says the dispatch record already landed in the claim
 * tx). So:
 *
 *  - **`dispatched`** — the window's open edge. `task.dispatched` fired in the
 *    claim tx with `worker_card_id` still NULL, and the next write is the
 *    silent one. The sweep's `resume_dispatched` arm re-drives a stale
 *    `dispatched` row into the same silent stamp. Nothing else will say so.
 *  - **`running`** — what that silent write produces. A row observed as
 *    `dispatched` must keep looking until it turns up, and a page opened
 *    mid-spawn may never have seen `dispatched` at all.
 *
 * And the two words that are **not** here, each for its own reason:
 *
 *  - **`pending`** is excluded because its only exit is evented: the claim tx
 *    emits `Event::TaskDispatched`, and `task.dispatched` is in
 *    `taskVerdictInvalidatingKinds` (`core/events/invalidation-plan`). A timer
 *    buys nothing there — and it costs without limit, because nothing moves a
 *    pending row on a schedule. A task behind a zero task budget, behind a
 *    dependency that failed, or left pending by a canceled track stays pending
 *    for as long as the track exists, and the track page would refetch the whole
 *    document projection every 3 s for as long as it is open.
 *  - **`verifying`** is excluded because it is evented on both sides. It is
 *    only ever *entered* by `task_report_success_from_worker_tx`, whose two
 *    call sites (`decision_sink.rs`, `scheduler/mod.rs`) both emit
 *    `Event::TaskCompleted` in the same tx; and it is only left through
 *    `reconcile_gate_outcome`'s `task.gate_result` / `task.completed` /
 *    `task.failed`. All four kinds invalidate this query. By the time a row
 *    reads `verifying` its `worker_card_id` is long since stamped and was
 *    delivered by that entry event — there is nothing left for a poll to find,
 *    and a gate can sit parked for hours.
 *
 * Written as an **allowlist**, not as "anything that is not done/failed/
 * canceled", and the difference is the failure mode. A status this build has
 * not heard of is either a new in-flight word or a new terminal one; under a
 * denylist a new terminal word would leave every finished track refreshing
 * forever, while under this allowlist a new in-flight word only costs the
 * liveness this build already lacked. The unknown case must degrade toward
 * silence, because the caller is a timer.
 */
function eventlessWindowTaskStatuses(): ReadonlySet<string> {
  return new Set(['dispatched', 'running']);
}

/**
 * Does any **row this panel actually draws** describe a run that is still in
 * flight?
 *
 * This exists because **the kernel emits no event for the one write the panel
 * most needs**. `scheduler::mark_running` flips `dispatched → running` and
 * stamps `worker_card_id` in a plain guarded UPDATE with no event riding along
 * (its own comment says so: the dispatch record already landed in the claim
 * tx). `task.dispatched` fires *before* the spawn, when `worker_card_id` is
 * still NULL, and every `runtime.*` event a worker adapter emits is emitted
 * during the spawn — also before that stamp. So from spawn until the task
 * reaches `task.completed` / `task.failed` there is no event at all for a
 * terminal worker, and only `codex.hook` / `claude.hook` for the agent ones —
 * which this frontend deliberately does not let invalidate the report query
 * (see `core/events/invalidation-plan`'s `taskVerdictInvalidatingKinds`: a hook
 * fires about twice per tool call per worker and provably cannot change a
 * `tasks` row). The result without a timer is that the click-through to the
 * worker card is dead for exactly the window a reader wants it.
 *
 * The answer is a bounded poll rather than a new kernel event: `mark_running`'s
 * silence is a considered decision, `worker_card_id` is exposed under the #1030
 * read-state exception (`task_projection.rs`) rather than pushed, and adding an
 * `Event` kind here would move the kernel's event surface — schema, policies,
 * goldens — to converge a cosmetic column. A poll converges identically for all
 * three worker kinds and costs nothing outside that window, which is what this
 * predicate decides. A verdict with no status at all is *not* live: "declared,
 * never dispatched" leaves the `tasks` row absent, and the write that creates
 * it (`task.dispatched`) is evented.
 *
 * "Outside that window" is narrower than "terminal" — see
 * `eventlessWindowTaskStatuses` for why `pending` and `verifying` are not live
 * for this purpose even though the kernel can still move them. Both can sit
 * unmoved indefinitely, and both are bracketed by events, so counting them
 * would turn a bounded poll into an unbounded one.
 *
 * **It reads rows, not verdicts, and that is the whole of its correctness.**
 * "Costs nothing outside that window" is a claim about a timer whose only
 * purpose is to make something on screen converge, so the thing it asks about
 * must be something on screen. A verdict is not: the kernel synthesises one for
 * a *deleted* declaration whose row is still in flight (`blockId: ''`, matching
 * no block in this report, and no row is built for it), so an in-flight status
 * on it would have kept the 3 s refetch running against a panel that will never
 * show a difference. Rows are what the reader is waiting on, so rows are what
 * the timer waits for.
 */
export function hasLiveTaskRun(rows: readonly ReportTaskRow[] | undefined): boolean {
  if (rows === undefined) return false;
  const live = eventlessWindowTaskStatuses();
  return rows.some((row) => row.status !== null && live.has(row.status));
}

/** The track's task verdicts. Named for what it reads, not for the route: the
 *  route also answers with the report, which this deliberately discards. */
export function trackTaskVerdictsOperation(trackId: string): ApiOperation<TaskVerdict[]> {
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/report`,
    responseSchema: trackReportReadSchema,
  };
}

/** The three worker kinds a live task declaration can name. Mirrors
 *  `liveTaskBlockPayloadSchema`'s `kind`, which is where the value comes from —
 *  a verdict does not carry one. */
export type TaskWorkerKind = 'codex' | 'claude' | 'terminal';

/**
 * One row of the panel's task inventory.
 *
 * **The three facts travel separately, because they are read separately.**
 * This used to be one `note` string that the join formatted — `Withdrawn`, or
 * `running · codex`, or `failed` — and one string was the right shape only
 * while the whole column was one word in one rank. It is not any more (#1149):
 * the status is a carrier of its own at the row's trailing edge — a shaped,
 * coloured dot on the desktop, the bare word on mobile (#1234 S1b-4b) — the worker kind
 * is the *only* control that opens the worker card, and the declaration word is
 * neither of those. A renderer handed `'running · codex'` would have to split
 * that string back apart to give its two halves different behaviour, which is
 * the join's knowledge leaking into presentation through a format.
 *
 * So the module keeps deciding *which* facts a row may carry — that is the
 * whole point of the withdrawn / unreadable rules below — and stops
 * deciding how they are spelled next to each other.
 */
export type ReportTaskRow = Readonly<{
  /** The block to reveal in the document — the detail lives there, not here. */
  blockId: string;
  key: string;
  state: ReportTaskState;
  /**
   * The word the *declaration* carries: `Not ready`, `Withdrawn`, `Unreadable`.
   * Never a runtime word, and `null` both for the ordinary case (declared,
   * ready) and for a row that has a `status` — a run supersedes the readiness
   * word exactly as it did when the two shared one slot.
   */
  declaration: string | null;
  /**
   * The kernel's `tasks` row status — `pending` / `running` / `done` / … — or
   * `null` when there is no run this row may report (never dispatched,
   * withdrawn, unreadable, or a key two live rows claim). Printed as it stands:
   * a status this build has not heard of is more useful shown than hidden.
   */
  status: string | null;
  /**
   * The kernel's reason for that status, or `null`. Only ever set alongside a
   * `status`: it *qualifies* the status word (`failed — track … is not a git
   * repository`), and a reason with no state to attach it to would be a claim
   * about a run this row is not allowed to report at all.
   *
   * Already collapsed to one line and bounded to
   * `TASK_STATUS_DETAIL_LIMIT` here rather than at the renderer — see
   * `boundedStatusDetail`.
   */
  statusDetail: string | null;
  /**
   * The worker kind, off the *declaration* — a withdrawn or unreadable block
   * has none. It is a fact about the task whether or not anything has run,
   * which is why it does not wait for a verdict.
   */
  kind: TaskWorkerKind | null;
  /** The worker card this task is running on, or `null`. It is what makes the
   *  row's kind a control rather than a label, because "which card is doing
   *  this" is the question a running task raises and the document cannot
   *  answer. */
  workerCardId: string | null;
  /** A server-rendered, actionable pending/admission explanation. */
  pendingReason: TaskPendingReason | null;
}>;

/**
 * How much of the kernel's reason a row may carry.
 *
 * The bound is not cosmetic and is therefore not left to CSS. The detail's only
 * destination is the status dot's accessible name and its `title`, and neither
 * can be truncated by a stylesheet: a screen reader reads the whole `aria-label`
 * however long it is, and a browser tooltip is not styleable at all. An
 * unbounded kernel message — these are full sentences, and a spawn failure can
 * quote a whole command line — would turn a one-word status into a paragraph
 * announced on every row visit, which is worse than not carrying it.
 *
 * 160 is a sentence: long enough for every message the kernel writes today
 * (`track <uuid> is not a git repository` is 45), short enough that the tail is
 * still a tooltip. The report block is where an untruncated reason belongs, and
 * the row already reveals it on click.
 */
export const TASK_STATUS_DETAIL_LIMIT = 160;

/**
 * The kernel's reason, as one bounded line — or `null` when there is nothing to
 * say.
 *
 * Whitespace is collapsed before the bound, not after: a message with a newline
 * in it (a quoted stderr line) would otherwise spend its budget on layout that
 * an accessible name cannot render anyway, and a `title` prints the newline
 * literally. Empty-after-trim is `null` and not `''` for the same reason the
 * empty `workerCardId` is: a blank reason renders as a dangling separator that
 * says the kernel spoke when it did not.
 */
function boundedStatusDetail(raw: string | null | undefined): string | null {
  if (typeof raw !== 'string') return null;
  const collapsed = raw.replace(/\s+/g, ' ').trim();
  if (collapsed === '') return null;
  if (collapsed.length <= TASK_STATUS_DETAIL_LIMIT) return collapsed;
  /*
   * The bound counts UTF-16 code units, because that is what `length` counts
   * and what the limit above was priced in. Slicing on one, though, can land
   * *inside* an astral character: a reason whose emoji straddles offset 159
   * would be cut between its surrogates, and a lone surrogate is not a
   * character — it renders as `�` in the tooltip and is announced as one in the
   * accessible name. So a trailing high surrogate with nothing to pair with is
   * dropped before the ellipsis is appended; the result is one code unit
   * shorter, never longer, so the bound still holds.
   */
  const cut = collapsed.slice(0, TASK_STATUS_DETAIL_LIMIT - 1);
  const head = /[\uD800-\uDBFF]$/.test(cut) ? cut.slice(0, -1) : cut;
  return `${head.trimEnd()}…`;
}

/**
 * The word, from the declaration alone. `ready` is silent on purpose: a column
 * in which every row carries a word is a column nobody reads.
 */
function declarationWord(state: ReportTaskState): string | null {
  if (state === 'ready') return null;
  if (state === 'withdrawn') return 'Withdrawn';
  if (state === 'unreadable') return 'Unreadable';
  return 'Not ready';
}

/*
 * **There was a `runtimeNote` here, and it is gone.**
 *
 * It formatted the status and the worker kind into one string —
 * `` `${status} · ${kind}` `` when the task had a card, the bare status
 * otherwise, and `failed` alone because a failure outranked the card it failed
 * on. Every one of those rules was a *typographic* decision about how two facts
 * share one slot, made in the module that is not allowed to know there is a
 * slot. They stop being decidable here the moment the two facts render as
 * different things (a dot and a control), so they are not restated in some
 * other form: both facts are simply carried, and the panel says where each one
 * goes. `failed` no longer needs to outrank anything — it is a red dot at the
 * row's edge, which is louder than any word ever was.
 *
 * **What survives is not typography and is still here**, in `deriveReportTasks`
 * below: the frontend never invents a reason from `schedulable`. An earlier cut
 * split `pending` on that boolean and called the false half `blocked`, even
 * though false also covers ceiling/tree admission. The server now returns the
 * tagged `pendingReason` after evaluating dependencies, effective task budget,
 * and admission diagnostics. This join carries its message verbatim; absence
 * stays silent instead of becoming a fourth client-side scheduler rule.
 */

/**
 * Index the verdicts twice, and look up by block id first.
 *
 * `blockId` is an identity join — the verdict names the very block the row was
 * derived from. `key` is the fallback, for the case where the report card's
 * blocks and the projection's block ids have drifted (the card payload and the
 * projection are written by different paths, and a stale card is the ordinary
 * state between an edit and its refetch), and for a verdict the kernel
 * synthesised for a *deleted* declaration, which carries `blockId: ''`.
 *
 * **Both must agree when both are present.** The kernel no longer stamps run
 * state by key alone — since #1160 `attach_task_read_state` attaches it to the
 * *one* block the declarations name as the key's owner, and abstains
 * (`status: null`) when several live blocks claim the key. What survives is
 * the reason the rule was needed in the first place: **one key still yields
 * several verdicts.** The projection emits one per declaration, so a tombstone
 * and its live re-declaration both arrive carrying the same key, and only one
 * of them carries the run. On top of that, the two halves of this join are two
 * different reads of two different snapshots — the blocks come with the track
 * detail, the verdicts from `['track-report', trackId]` — and `block_id` is not
 * a durable identity: it is minted by FNV-1a with linear probing and
 * re-inherited heuristically on a rewrite (`report_blocks/align.rs`), so a
 * hard-deleted block's id can be re-issued to a different declaration. A
 * block-id hit whose key contradicts the block's own declared key is therefore
 * not evidence about this row, and the key index is used *only* for a verdict
 * whose block id matches no block at all.
 *
 * **The key index is gated from both ends, and it takes both.**
 *
 * *Verdict side.* A verdict naming a block this report *does* have has already
 * said which row it is about; letting it also sit in the key index would let it
 * decorate a *different* row through the fallback whenever its own row happens
 * not to consult the index. Two rows `b-alpha`/`alpha` and `b-beta`/`beta` plus
 * one verdict `{blockId: 'b-beta', key: 'alpha'}` is the whole trigger:
 * `b-alpha` finds no verdict at its own id, falls back on its key, and reports
 * a run the verdict itself says belongs to `b-beta` — including routing the
 * click at `b-beta`'s worker card. So `rowBlockIds` keeps such a verdict out.
 *
 * *Row side.* That half is **not** sufficient, and an earlier cut of this
 * comment claimed it was ("only a verdict whose block id names no row at all
 * may ever be reached by key" — true, and beside the point). It only bites
 * while the block the verdict names is still in the document. Hard-delete that
 * block and paste **two** new ones on its key in the same edit, and the verdict
 * about the deleted block names no row any more: it enters the key index, and
 * *both* new rows — neither of which has a verdict at its own id — fall back
 * onto it, so one dead task's terminal status and worker card get painted onto
 * two rows at once. A terminal status is outside `eventlessWindowTaskStatuses`,
 * so nothing restarts the poll that would clear it either.
 *
 * Hence `keysClaimedByManyRows`: **a key more than one row in this render
 * claims is not a usable fallback for any of them.** That is a question about
 * the rows on screen — how many of them would consult this entry — and it is
 * answered by counting them, which is why it can live here. It is *not* the
 * removed `contestedLiveKeys` pre-pass in another spelling: that one re-read
 * the blocks to decide who *owns* a key and then overrode the kernel's own
 * answer, a second authority on ownership. This one never contradicts a
 * verdict; it declines to guess which of several rows an id-less verdict was
 * about, and the identity join is untouched — a row whose own block id is named
 * still shows its run, contested key or not.
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
 * The verdict that is about *this* block, or none.
 *
 * The block-id hit wins, but only if it does not contradict the declared key;
 * the key index is consulted only when no verdict names this block id, which is
 * what keeps a redeclared key from reporting the withdrawn block's run (and
 * vice versa) on both rows.
 *
 * **A miss here is a real answer, and both ways of missing are deliberate. The
 * row renders blank, and blank is the expected value** — this whole module's
 * position is that when it cannot tell which row a verdict is about, it says
 * nothing.
 *
 * *Missing through the key index.* `keysDeclaredByMoreThanOneRow` counts
 * tombstoned declarations too, so a withdrawn `alpha` beside its single live
 * re-declaration already puts `alpha` out of the index — and the kernel's
 * synthesised `{blockId: '', key: 'alpha'}` verdict then reaches neither row,
 * including the live one that is the key's only live claimant. That cost is
 * accepted twice over. An id-less verdict means precisely *"when the kernel
 * looked, no live declaration owned this key"*; a live row appearing in the
 * document afterwards is not evidence that the old run was that row's, so
 * handing it over would be a guess. And the rule is kept **purely syntactic on
 * purpose**: it asks only how many rows of *this* render declared the key
 * (`isTaskBlock` + `declaredTaskKey`) and knows nothing of live/tombstoned or
 * of ownership, so it cannot drift from the kernel. Teaching it to skip
 * tombstones would drag the live/tombstone decision back into this file — the
 * exact shape of the `contestedLiveKeys` pre-pass #1160 removed — and that
 * decision is subtle here (`taskRowState` below: `tombstoned_by` is the
 * discriminant, a live block may carry `tombstone: null`), so a second copy of
 * it would not fail loudly when the kernel changes its representation. It would
 * quietly answer differently.
 *
 * *Missing through a contradicting block-id hit.* When a verdict names this
 * block id but carries another key, this returns nothing and does **not** fall
 * back to the key index — even when the key index holds an entry for the
 * declared key. Block ids are re-issued (`report_blocks/align.rs` mints by
 * FNV-1a with linear probing, so a hard-deleted block's id can land on a new
 * declaration), so a stale verdict about the *old* block can occupy the new
 * block's id and shadow the verdict that would have arrived by key. The row
 * goes blank. That is the fail-closed direction and it is a consequence of a
 * fact that predates this fix: a hit that contradicts itself is not evidence
 * about this row, and consulting the key index *after* seeing one would mean
 * treating a self-contradicting index as a reason to trust a second index.
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
 * The row's state, from the declaration alone.
 *
 * `tombstoned_by` is the discriminant, not `tombstone`: a live task may carry
 * an explicit `tombstone: null`, so the key's presence proves nothing. Same
 * test as `features/report/task`.
 *
 * The row loop below is the only caller left. It used to be two — a
 * pre-pass also counted live claims per key, so that a key two live blocks
 * both declared could be refused decoration here (#1159). The kernel now
 * answers that itself: `attach_task_read_state` attaches run state only to the
 * *single* live declaration of a key, so a contested key arrives as
 * `status: null` on the wire and there is nothing left for this module to
 * second-guess (#1160).
 *
 * The pre-pass read the *blocks*, which are a different snapshot from the
 * verdicts, so dropping it does widen one window: a cached run can outlive the
 * moment a second live block appeared on its key. That is staleness of a
 * block-id-joined fact about the very row that shows it, never a run borrowed
 * from a neighbour — see `report.test.ts`'s "keeps a run on the block the
 * kernel gave it to when the verdicts lag the blocks" for the sequence and for
 * why re-deriving contention here would only re-introduce a second authority
 * on ownership.
 */
function taskRowState(block: ReportBlock): ReportTaskState {
  if (block.kind !== 'task') return 'unreadable';
  if ('tombstoned_by' in block.payload) return 'withdrawn';
  return block.payload.ready ? 'ready' : 'not-ready';
}

/** The key the block itself declared, or `''` for one this build cannot read.
 *  Only a declared key may look a verdict up: the row's *display* name falls
 *  back to the block id, and matching **that** against the key index would let
 *  one task's run be reported on another task's row. */
function declaredTaskKey(block: ReportBlock): string {
  return block.kind === 'task' ? block.payload.key : '';
}

/**
 * The keys two or more rows of this render declare.
 *
 * Every task block counts, withdrawn ones included: a tombstone beside its live
 * re-declaration is two rows on one key, and an id-less verdict on that key is
 * as unattributable there as it is between two live blocks — the kernel emits
 * one verdict per declaration, so it has already said the key alone does not
 * pick a row out. **Not filtering tombstones out is the deliberate half**, and
 * it does cost something: a tombstone plus the key's *only* live declaration
 * still suppresses the fallback, so the kernel's `{blockId: ''}` verdict paints
 * neither row and the live one renders blank. Blank is the expected value —
 * see `verdictFor` for why that is not a guess worth making, and for why this
 * rule is kept purely syntactic (`isTaskBlock` + `declaredTaskKey`, no
 * live/tombstoned judgement) so it cannot drift from the kernel.
 *
 * `''` is skipped because `verdictFor` refuses to look an undeclared key up in
 * the first place (the display name falls back to the block id, and matching
 * *that* against the key index is its own defect), so counting it would decide
 * nothing.
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
 * Every `task` block, in document order, as one row each, decorated with
 * whatever the kernel's task projection says about it.
 *
 * Withdrawn tasks are kept, for the same reason the block itself keeps them:
 * the task existed, other reports may cite its block id, and a list that
 * silently dropped it would disagree with the document it is derived from.
 *
 * `verdicts` is optional and defaults to none: the report card is in hand on
 * the first render and the verdict read lands later, so "no verdicts yet" must
 * render the same list the declarations alone produced rather than a hole.
 */
export function deriveReportTasks(
  blocks: readonly ReportBlock[] | null,
  verdicts: readonly TaskVerdict[] = [],
): ReportTaskRow[] {
  if (blocks === null) return [];
  /* Every block that will become a row, before any of them is looked up: the
     key index is only for verdicts about blocks this report does not have, and
     that predicate is not decidable one block at a time. */
  const rowBlockIds = new Set(blocks.filter(isTaskBlock).map((block) => block.id));
  /* And every key more than one of those rows declares: the fallback cannot
     say which of them an id-less verdict was about, so it answers none of
     them. Also not decidable one block at a time. */
  const keysClaimedByManyRows = keysDeclaredByMoreThanOneRow(blocks);
  const index = indexVerdicts(verdicts, rowBlockIds, keysClaimedByManyRows);
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
    const isReadable = block.kind === 'task';
    const state = taskRowState(block);
    /* The declared key, before the id fallback below. */
    const declaredKey = declaredTaskKey(block);
    /* `kind` lives on the live declaration, not on the verdict — the same
       field `features/report/task` prints. A withdrawn or unreadable task has
       none, and its row is the one that offers no worker card either: the two
       are the same fact, which is why the panel can hang the card control off
       the kind and be sure it never appears on a struck row. */
    const kind: TaskWorkerKind | null = isReadable && !('tombstoned_by' in block.payload)
      ? block.payload.kind
      : null;
    /*
     * A row always has a name. `key` is `z.string()` — the kernel does not
     * require it to be non-empty — and an empty one reaches the panel as a
     * button with no text and therefore no accessible name: unreadable to a
     * screen reader, invisible to a pointer, and a `getByRole('button')` that
     * matches nothing. The same fallback the unreadable branch uses applies,
     * for the same reason: the block id is the literal other reports cite this
     * block by, which is the one name it always has.
     */
    const key = declaredKey === '' ? block.id : declaredKey;
    /*
     * A withdrawn or unreadable declaration takes no runtime decoration at all.
     *
     * Withdrawal does not delete the `tasks` row of a task that was already
     * dispatched, so its verdict keeps reporting `status: 'done'` (or
     * `running`) long after the block was struck. Letting that word win made
     * the panel print an unstruck `done` and lose the word `Withdrawn`
     * entirely — the one fact this row exists to carry — and flipped the click
     * to the worker card, so the withdrawn block could no longer be revealed
     * from the panel at all. `unreadable` is the same class: this build cannot
     * read the block's key, so it cannot vouch that a verdict is about it.
     *
     * The task existed and was withdrawn; that is what the panel owes the
     * reader.
     *
     * A key two live blocks both claim used to be refused here as well, for a
     * different reason: not that this row cannot be vouched for, but that
     * *neither* row can. That rule now lives in the kernel — one `tasks` row
     * has at most one live declaration that may carry it, and an ambiguous key
     * is answered `status: null` for every block that names it (#1160) — so
     * this build no longer re-derives it from the document.
     */
    const decorated = state !== 'withdrawn' && state !== 'unreadable';
    const verdict = decorated ? verdictFor(block.id, declaredKey, index) : undefined;
    /*
     * `''` is not a status, for the same reason `''` is not a card id two
     * fields below. The wire types it as an optional string, so an empty one is
     * not `null` and would pass every gate written against `null`: it would
     * silence the declaration word (`Withdrawn` is lost, and the row says
     * nothing at all), let a `statusDetail` through with no state to qualify —
     * `Status:  — boom`, which both `taskStatusPhrase` and `ReportTaskRow`
     * document as the one shape they may not produce — and render as
     * `data-nc-status=""`, which matches no dot form and so paints the
     * neutral ring while claiming a run exists. Today's kernel serialises
     * `TaskStatus` to a fixed lowercase word and cannot emit this; the wire
     * type is what is being hardened against, not the current writer.
     */
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
      /* No `tasks` row means nothing has run, and inventing a word for that is
         exactly the thing this list was criticised for not being able to do
         honestly. The declaration word stands — and it stands *down* once there
         is a status, which is the one precedence rule the old single `note`
         encoded that is still a fact about meaning rather than about layout:
         `Not ready` describes a declaration the kernel has since dispatched
         anyway, and printing both would show the row arguing with itself. */
      declaration: status === null ? declarationWord(state) : null,
      status,
      /* Gated on `status`, not on the verdict: the detail is a qualifier on the
         status word and has nowhere to attach without one. That also makes it
         inherit every rule above for free — a withdrawn or unreadable row
         has no `status`, so it cannot leak a reason either. */
      statusDetail: status === null ? null : boundedStatusDetail(verdict?.statusDetail),
      kind,
      /* `''` is not a card id. The wire types it as an optional string and an
         empty one would route a click at a card that cannot exist. */
      workerCardId: verdict?.workerCardId === undefined || verdict.workerCardId === null
        || verdict.workerCardId === '' ? null : verdict.workerCardId,
      pendingReason,
    });
  }
  return rows;
}

/* ── Backlinks ──────────────────────────────────────────────────────────
   Who cites this track. The kernel resolves `neige://wave/<id>#<block>` links
   found in other tracks' reports and hands back a bounded page. */

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

/** `truncated` and `skipped_sources` are read, not dropped: a backlink list
 *  that is knowingly incomplete must say so (§8.3) — silently short is the one
 *  failure mode a citation list may not have. */
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

/** Group by source track, preserving server order — the panel prints one
 *  heading per citing track, not one per citation. */
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

/** How many backlinks land on each block, for the sidenote markers (§8.3). */
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

/* ── `neige://` links ───────────────────────────────────────────────────
   A report cites another track with `neige://wave/<id>[#<block id>]`. The
   destination is resolved here, in core, so the renderer never has to hold a
   URL: it gets a track id and an optional block id, or nothing. */

const NEIGE_WAVE_LINK = /^neige:\/\/wave\/([^/?#]+)(?:#([^#]+))?$/;

/** Block ids the kernel mints. A link whose fragment is not one of these keeps
 *  the track and drops the fragment: landing at the top of the right report
 *  beats a dead link. */
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
