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

export const chartCandlesPayloadSchema = z.object({
  symbol: z.string(),
  period: z.enum(['day', 'week', 'month']).optional(),
  /** Data is inlined; range switching is a pure client-side filter. */
  candles: z.array(candleTupleSchema).min(2),
  overlays: z.array(z.enum(['ma20', 'ma60'])).optional(),
  caption: z.string().optional(),
});

export const tableBlockPayloadSchema = z.object({
  columns: z
    .array(
      z.object({
        key: z.string(),
        label: z.string(),
        align: z.enum(['left', 'right']).optional(),
      }),
    )
    .min(1),
  rows: z.array(
    z.record(z.string(), z.union([z.string(), z.number(), z.null()])),
  ),
  caption: z.string().optional(),
  highlight: z.string().optional(),
});

export const appBlockPayloadSchema = z.object({
  /** Same-origin absolute path — `//host` protocol-relative URLs are not. */
  src: z
    .string()
    .refine((s) => s.startsWith('/') && !s.startsWith('//'), {
      message: 'src must be a same-origin absolute path',
    }),
  title: z.string().optional(),
  height: z.number().min(120).max(2000).optional(),
});

export type ProseBlockPayload = z.infer<typeof proseBlockPayloadSchema>;
export type ChartCandlesPayload = z.infer<typeof chartCandlesPayloadSchema>;
export type TableBlockPayload = z.infer<typeof tableBlockPayloadSchema>;
export type AppBlockPayload = z.infer<typeof appBlockPayloadSchema>;

const blockCommon = {
  id: z.string(),
  rev: z.number().int(),
};

const typedReportBlockSchema = z.discriminatedUnion('kind', [
  z.object({ ...blockCommon, kind: z.literal('prose'), payload: proseBlockPayloadSchema }),
  z.object({
    ...blockCommon,
    kind: z.literal('chart.candles'),
    payload: chartCandlesPayloadSchema,
  }),
  z.object({ ...blockCommon, kind: z.literal('table'), payload: tableBlockPayloadSchema }),
  z.object({ ...blockCommon, kind: z.literal('app'), payload: appBlockPayloadSchema }),
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
      blocks: parsed.data.blocks,
      updatedAt: k.updated_at,
    };
  },
};
