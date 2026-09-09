// #1595: persisted composition, independent of a particular dashboard or Track.
// The Rust validator in report_blocks/layout.rs owns the same write contract.
import { z } from 'zod';

const text = z.string().refine(value => [...value].length <= 2048, 'Text exceeds 2048 code points');
const key = z.string().min(1).refine(value => [...value].length <= 2048, 'Key exceeds 2048 code points');
const scalar = z.union([text, z.number().finite(), z.null()]);
// z.record strips an own __proto__ key. Report rows are scalar JSON records:
// validate every entry and define own properties so accepted keys roundtrip.
const row = z.custom<Record<string, z.infer<typeof scalar>>>(value => value !== null && typeof value === 'object' && !Array.isArray(value))
  .superRefine((value, ctx) => {
    const entries = Object.entries(value);
    if (entries.length > 32) ctx.addIssue({ code: 'custom', message: 'Too many fields' });
    for (const [name, item] of entries) {
      if (!key.safeParse(name).success) ctx.addIssue({ code: 'custom', path: [name], message: 'Invalid row key' });
      if (!scalar.safeParse(item).success) ctx.addIssue({ code: 'custom', path: [name], message: 'Row values must be bounded scalar values' });
    }
  }).transform(value => Object.fromEntries(Object.entries(value)));
const selector = z.strictObject({ key, value: scalar });
const annotations = z.strictObject({ keys: z.array(key).min(1).max(4), rows: z.array(row).max(500) })
  .superRefine((value, ctx) => {
    if (new Set(value.keys).size !== value.keys.length) ctx.addIssue({ code: 'custom', message: 'Duplicate join keys' });
    const seen = new Set<string>();
    for (const entry of value.rows) {
      if (value.keys.some(name => !Object.hasOwn(entry, name) || entry[name] === null)) {
        ctx.addIssue({ code: 'custom', message: 'Annotation join keys must be present and non-null' });
      }
      const tuple = JSON.stringify(value.keys.map(name => entry[name]));
      if (seen.has(tuple)) ctx.addIssue({ code: 'custom', message: 'Duplicate annotation tuple' });
      seen.add(tuple);
    }
  });
const data = z.union([
  z.strictObject({ source: key.refine(value => !/\s/.test(value) && /^neige:\/\/plugin\/[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/.test(value), 'Invalid live source'), annotations: annotations.optional() }),
  z.strictObject({ rows: z.array(row).max(500) }),
]);
const unit = z.strictObject({ key, equals: key, row: selector.optional() });
const column = z.strictObject({
  key, label: text, format: z.enum(['text', 'number', 'percent', 'share']), digits: z.number().int().min(0).max(8),
  fallbackKey: key.optional(), suffixKey: key.optional(), linkKey: key.optional(),
});
const dayRange = z.number().int().min(1).max(3660);

export const layoutChartSchema = z.strictObject({
  kind: z.literal('chart'), title: text, span: z.number().int().min(1).max(3), data, exclude: selector.optional(),
  chart: z.enum(['line', 'donut']), x: key, y: key, height: z.number().int().min(160).max(640),
  labelSuffixKey: key.optional(),
  color: z.string().length(7).regex(/^#[0-9A-Fa-f]{6}$/), unit: unit.optional(),
  ranges: z.array(dayRange).min(1).max(8).optional(), defaultRange: dayRange.optional(),
}).superRefine((value, ctx) => {
  if (value.labelSuffixKey !== undefined && value.chart !== 'donut') ctx.addIssue({ code: 'custom', message: 'Only donut labels accept a suffix' });
  if ((value.ranges === undefined) !== (value.defaultRange === undefined)
    || (value.ranges !== undefined && (value.chart !== 'line' || !value.ranges.includes(value.defaultRange!)
      || value.ranges.some((entry, index) => index > 0 && entry <= value.ranges![index - 1])))) {
    ctx.addIssue({ code: 'custom', message: 'Line ranges must be ascending, unique and include defaultRange' });
  }
});
export const layoutTableSchema = z.strictObject({
  kind: z.literal('table'), title: text, span: z.number().int().min(1).max(3), data, exclude: selector.optional(),
  columns: z.array(column).min(1).max(32), total: z.strictObject({ row: selector, key }).optional(),
}).superRefine((value, ctx) => {
  if (new Set(value.columns.map(item => item.key)).size !== value.columns.length) ctx.addIssue({ code: 'custom', message: 'Duplicate column key' });
  if (value.columns.some(item => item.format === 'share') !== (value.total !== undefined)) {
    ctx.addIssue({ code: 'custom', message: 'Share columns require an explicit total, and only share uses a total' });
  }
});
export const reportLayoutSchema = z.strictObject({
  version: z.literal(1), columns: z.number().int().min(1).max(3), gap: z.enum(['compact', 'normal', 'wide']),
  surface: z.enum(['plain', 'muted']), items: z.array(z.union([layoutChartSchema, layoutTableSchema])).min(1).max(12),
}).superRefine((value, ctx) => {
  if (value.items.some(item => item.span > value.columns)) ctx.addIssue({ code: 'custom', message: 'Item span exceeds layout columns' });
});

export type ReportLayout = z.infer<typeof reportLayoutSchema>;
export type LayoutChart = z.infer<typeof layoutChartSchema>;
export type LayoutTable = z.infer<typeof layoutTableSchema>;
export type LayoutItem = ReportLayout['items'][number];
export type LayoutRow = z.infer<typeof row>;
export type LayoutColumn = z.infer<typeof column>;
export type LayoutSelector = z.infer<typeof selector>;

// A live producer uses a native table-shaped overlay; presentation columns are
// not reinterpreted here. Strict row bounds and scalar values still apply.
export const layoutLiveDataSchema = z.object({ rows: z.array(row).max(500), caption: text.nullish() });
