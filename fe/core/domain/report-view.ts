import { z } from 'zod';
import { isCalendarDate } from './report-date.js';
import { inlineTableBlockPayloadSchema } from './report-table.js';

const text = (limit = 2048) => z.string().refine(v => [...v].length <= limit, 'Text limit exceeded').meta({ maxLength: limit });
const id = z.string().min(1).max(100).regex(/^[A-Za-z0-9._-]+$/);
const number = z.number().min(-1e15).max(1e15);
const date = z.string().refine(v => !v.startsWith('0000') && isCalendarDate(v), 'Expected a UTC calendar date').meta({ format: 'date' });
const tone = z.enum(['neutral', 'positive', 'warning', 'negative']);
const palette = z.number().int().min(1).max(6);
const fields = z.array(z.strictObject({ label: text(120), value: text() })).max(12);
const status = z.strictObject({ label: text(120), tone });
const value = z.discriminatedUnion('state', [
  z.strictObject({ state: z.literal('known'), amount: number, unit: text(32),
    placement: z.enum(['prefix', 'suffix']), decimals: z.number().int().min(0).max(4), signed: z.boolean() }),
  z.strictObject({ state: z.literal('unknown'), reason: text(500) }),
]);
const metric = z.strictObject({ id, label: text(120), value, detail: text(500), tone, emphasis: z.enum(['primary', 'normal']) });
const unique = (items: readonly { id: string }[]) => new Set(items.map(item => item.id)).size === items.length;
const series = z.strictObject({ id, label: text(120), palette });
const dataset = z.strictObject({ id, label: text(120), unit: text(32), style: z.enum(['line', 'stacked']),
  series: z.array(series).min(1).max(6).refine(unique, 'Duplicate series'),
  points: z.array(z.strictObject({ date, values: z.array(number.nullable()).min(1).max(6) })).max(500),
}).superRefine((data, ctx) => {
  for (let i = 0; i < data.points.length; i++) {
    const point = data.points[i];
    if (point.values.length !== data.series.length) ctx.addIssue({ code: 'custom', message: 'Point width must match series' });
    if (i > 0 && point.date <= data.points[i - 1].date) ctx.addIssue({ code: 'custom', message: 'Dates must increase' });
    if (data.style === 'stacked' && point.values.some(v => v === null || v < 0)) ctx.addIssue({ code: 'custom', message: 'Stacked data must be complete and nonnegative' });
  }
});
const record = z.strictObject({ id, category: text(120), title: text(200), summary: text(),
  status, handling: status, facts: fields,
  sections: z.array(z.strictObject({ label: text(120), body: text() })).max(8),
  evidence: z.array(z.strictObject({ id, label: text(200), date, body: text(), note: text(500), tone })).max(20)
    .refine(unique, 'Duplicate evidence'),
});
export const nativeComponentSchema = z.discriminatedUnion('kind', [
  z.strictObject({ kind: z.literal('metrics'), id, title: text(200), items: z.array(metric).min(1).max(8)
    .refine(unique, 'Duplicate metric').refine(items => items.filter(i => i.emphasis === 'primary').length <= 1, 'At most one primary metric') }),
  z.strictObject({ kind: z.literal('time-series'), id, title: text(200), caption: text(500), emptyText: text(500),
    datasets: z.array(dataset).min(1).max(4).refine(unique, 'Duplicate dataset') }),
  z.strictObject({ kind: z.literal('distribution'), id, title: text(200), unit: text(32), emptyText: text(500),
    slices: z.array(z.strictObject({ id, label: text(120), value: number.nonnegative(), palette })).max(12).refine(unique, 'Duplicate slice') }),
  z.strictObject({ kind: z.literal('table'), id, title: text(200), table: inlineTableBlockPayloadSchema }),
  z.strictObject({ kind: z.literal('records'), id, title: text(200), emptyText: text(500),
    datasets: z.array(z.strictObject({ id, label: text(120), items: z.array(record).max(50).refine(unique, 'Duplicate record') }))
      .min(1).max(4).refine(unique, 'Duplicate dataset') }),
]);
export const nativeViewPayloadSchema = z.strictObject({
  version: z.literal(1), title: text(200), description: text(500),
  snapshot: z.strictObject({ id, observedAt: z.number().int().min(0).max(253402300799999), producedAt: z.number().int().min(0).max(253402300799999) }),
  rows: z.array(z.strictObject({ id, title: text(200), layout: z.enum(['one', 'two', 'three']),
    cells: z.array(nativeComponentSchema).min(1).max(3),
  }).refine(row => row.cells.length === ({ one: 1, two: 2, three: 3 })[row.layout], 'Layout must match cell count')).min(1).max(6)
    .refine(unique, 'Duplicate row'),
}).refine(view => unique(view.rows.flatMap(row => row.cells)), 'Duplicate component')
  .refine(view => {
    let bytes = 0;
    for (const char of JSON.stringify(view)) {
      const point = char.codePointAt(0)!;
      bytes += point < 128 ? 1 : point < 2048 ? 2 : point < 65536 ? 3 : 4;
    }
    return bytes <= 256 * 1024;
  }, 'View exceeds 256 KiB');

export type NativeViewPayload = z.infer<typeof nativeViewPayloadSchema>;
export type NativeComponent = z.infer<typeof nativeComponentSchema>;
