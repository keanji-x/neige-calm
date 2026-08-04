import { z } from 'zod';
import type { CardEntry } from '../registry';
import {
  WAVE_REPORT_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';

declare module '../../types' {
  interface WaveCardDataMap {
    'wave-report': WaveReportCardData;
  }
}

export interface WaveReportCardData {
  type: 'wave-report';
  title?: string | null;
  id?: string;
  summary: string;
  body: string;
  docRev: number;
  blocks?: ReportBlock[];
  updatedAt?: number;
  unsupportedVersion?: number;
}

/* ── Typed per-kind block payloads (issue #960 PR3) ──────────────────
   The backend (S1) emits blocks whose `kind` selects a typed JSON
   payload. The frontend parses the known kinds strictly; anything else
   (unknown kind OR a known kind whose payload fails its schema) falls
   through to the opaque catch-all below so one bad block never fails
   the whole card — the renderer degrades it to a placeholder. */

export const proseBlockPayloadSchema = z.object({
  markdown: z.string(),
});

/** One candle: [ts_ms, open, high, low, close, volume?]. */
export const candleTupleSchema = z.tuple([
  z.number(),
  z.number(),
  z.number(),
  z.number(),
  z.number(),
  z.number().optional(),
]);

// Caps mirror the Rust-side validation (crates, #960 review round 1) so a
// payload the kernel accepts is exactly what the frontend accepts — strict
// objects, bounded sizes. A payload violating any cap falls to the opaque
// catch-all (placeholder render), never failing the whole card.
export const chartCandlesPayloadSchema = z.strictObject({
  symbol: z.string().min(1).max(2048),
  period: z.enum(['day', 'week', 'month']).optional(),
  /** Data is inlined; range switching is a pure client-side filter. */
  candles: z.array(candleTupleSchema).min(2).max(5000),
  overlays: z.array(z.enum(['ma20', 'ma60'])).optional(),
  caption: z.string().max(2048).optional(),
});

export const tableBlockPayloadSchema = z
  .strictObject({
    columns: z
      .array(
        z.strictObject({
          key: z.string().min(1).max(2048),
          label: z.string().max(2048),
          align: z.enum(['left', 'right']).optional(),
        }),
      )
      .min(1)
      .max(32),
    rows: z
      .array(
        z.record(
          z.string(),
          z.union([z.string().max(2048), z.number(), z.null()]),
        ),
      )
      .max(500),
    caption: z.string().max(2048).optional(),
    highlight: z.string().max(2048).optional(),
  })
  .refine(
    (t) => new Set(t.columns.map((c) => c.key)).size === t.columns.length,
    { message: 'column keys must be unique' },
  )
  .refine(
    (t) => {
      const keys = new Set(t.columns.map((c) => c.key));
      return t.rows.every((row) =>
        Object.keys(row).every((k) => keys.has(k)),
      );
    },
    { message: 'row keys must be declared column keys' },
  );

export const appBlockPayloadSchema = z.strictObject({
  /** Same-origin absolute path: leading `/`, not protocol-relative `//`,
   *  and no backslashes (browsers normalize `\` to `/` in URLs, which
   *  would smuggle a `//host` past a prefix check). Mirrors the Rust
   *  validator; app.tsx additionally asserts the resolved origin. */
  src: z
    .string()
    .max(2048)
    .regex(/^\/(?!\/)[^\\]*$/, {
      message: 'src must be a same-origin absolute path',
    })
    // Mirror the Rust validator: no C0/C1 control characters (0x00..0x1F,
    // 0x7F..0x9F) — header-injection / log-forgery class characters have no
    // business in a same-origin path.
    .refine(
      (s) => {
        for (let i = 0; i < s.length; i++) {
          const c = s.charCodeAt(i);
          if (c < 0x20 || (c >= 0x7f && c <= 0x9f)) return false;
        }
        return true;
      },
      { message: 'src must not contain control characters' },
    ),
  title: z.string().max(2048).optional(),
  height: z.number().min(120).max(2000).optional(),
});

const taskGateStepSchema = z.strictObject({
  name: z.string(),
  cmd: z.string(),
});

const liveTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  kind: z.enum(['codex', 'claude', 'terminal']),
  goal: z.string(),
  acceptance: z.string().optional(),
  gate: z.strictObject({
    cwd: z.string().optional(),
    timeout_secs: z.number().int().optional(),
    steps: z.array(taskGateStepSchema),
  }).optional(),
  no_gate_reason: z.string().optional(),
  depends_on: z.array(z.string()).optional(),
  priority: z.number().int().optional(),
  cwd: z.string().optional(),
  context: z.unknown().optional(),
  refs: z.array(z.string()).optional(),
  ready: z.boolean(),
  declared_by: z.enum(['spec', 'user']),
  released_by_user: z.boolean().optional(),
  spawn: z.enum(['in-wave', 'sub-wave']).optional(),
  tombstone: z.null().optional(),
});

const tombstoneTaskBlockPayloadSchema = z.strictObject({
  key: z.string(),
  tombstone: z.strictObject({ reason: z.string().nullable().optional() }),
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

const blockCommon = {
  id: z.string(),
  rev: z.number().int(),
};

export const typedReportBlockSchema = z.discriminatedUnion('kind', [
  z.object({ ...blockCommon, kind: z.literal('prose'), payload: proseBlockPayloadSchema }),
  z.object({
    ...blockCommon,
    kind: z.literal('chart.candles'),
    payload: chartCandlesPayloadSchema,
  }),
  z.object({ ...blockCommon, kind: z.literal('table'), payload: tableBlockPayloadSchema }),
  z.object({ ...blockCommon, kind: z.literal('app'), payload: appBlockPayloadSchema }),
  z.object({ ...blockCommon, kind: z.literal('task'), payload: taskBlockPayloadSchema }),
]);

/** Catch-all: preserves the block (kind + raw payload) so the renderer
 *  can show an "unsupported" placeholder instead of failing the parse. */
const opaqueReportBlockSchema = z.object({
  ...blockCommon,
  kind: z.string(),
  payload: z.record(z.string(), z.unknown()),
});

export const reportBlockSchema = z.union([
  typedReportBlockSchema,
  opaqueReportBlockSchema,
]);

export type ReportBlock = z.infer<typeof reportBlockSchema>;

/** Strict zod schema for the wire payload. `schemaVersion` may be
 *  absent (treated as v1). */
export const waveReportPayloadSchema = z.object({
  schemaVersion: z.number().int().optional(),
  docRev: z.number().int().nonnegative().default(0),
  summary: z.string(),
  body: z.string(),
  blocks: z.array(reportBlockSchema).optional(),
});

export const WaveReportEntry: CardEntry<WaveReportCardData> = {
  type: 'wave-report',
  Component: () => null,
  defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
  claim: { mode: 'exact', kind: 'wave-report' },
  title: (card) => card.title || 'Report',
  accessibleName: (card) =>
    card.summary.trim().length > 0 ? `Report: ${card.summary}` : 'Report',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (k) => {
    if (k.kind !== 'wave-report') return null;
    const candidate = k.payload ?? {};
    const version = payloadSchemaVersion(candidate);
    if (version > WAVE_REPORT_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] wave-report payload schemaVersion=${version} unsupported (frontend supports ${WAVE_REPORT_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'wave-report',
        id: k.id,
        title: k.title,
        summary: '',
        body: '',
        docRev: 0,
        updatedAt: k.updated_at,
        unsupportedVersion: version,
      };
    }
    const parsed = waveReportPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] wave-report payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'wave-report',
      id: k.id,
      summary: parsed.data.summary,
      body: parsed.data.body,
      docRev: parsed.data.docRev,
      blocks: parsed.data.blocks,
      updatedAt: k.updated_at,
    };
  },
};
