import { z } from 'zod';
import { nativeViewShapeSchema } from './report-view.generated.js';
import type { NativeView, Component } from './report-view.types.generated.js';
import { inlineTableBlockPayloadSchema, rejectReservedObjectKeys } from './report-table.js';

export type NativeViewPayload = NativeView;
export type NativeComponent = Component;
const MAX_READ_BYTES = 4 * 1024 * 1024;

/** Independent decoded-resource budget; exact canonical write admission belongs to the kernel. */
function readBudget(value: unknown, ctx: z.RefinementCtx) {
  let encoded: string;
  try { encoded = JSON.stringify(value); } catch { ctx.addIssue({ code: 'custom', message: 'Expected JSON presentation' }); return; }
  if (encoded === undefined) { ctx.addIssue({ code: 'custom', message: 'Expected JSON presentation' }); return; }
  let bytes = 0;
  for (const char of encoded) {
    const point = char.codePointAt(0)!;
    bytes += point < 128 ? 1 : point < 2048 ? 2 : point < 65536 ? 3 : 4;
    if (bytes > MAX_READ_BYTES) { ctx.addIssue({ code: 'custom', message: 'Presentation exceeds 4 MiB read budget' }); return; }
  }
}

/** Relations between fields cannot be expressed by the structural JSON Schema.
 * The kernel independently enforces these on admission; no business decisions live here. */
function relations(view: NativeView, ctx: z.RefinementCtx) {
  const unique = (items: readonly { id: string }[], message: string) => {
    if (new Set(items.map(item => item.id)).size !== items.length) ctx.addIssue({ code: 'custom', message });
  };
  unique(view.rows, 'Duplicate row');
  unique(view.rows.flatMap(row => row.cells), 'Duplicate component');
  for (const row of view.rows) {
    const width = row.layout === 'one' ? 1 : row.layout === 'three' ? 3 : 2;
    if (row.cells.length !== width) ctx.addIssue({ code: 'custom', message: 'Layout must match cell count' });
    for (const component of row.cells) {
      switch (component.kind) {
        case 'metrics':
          unique(component.items, 'Duplicate metric');
          if (component.items.filter(item => item.emphasis === 'primary').length > 1) ctx.addIssue({ code: 'custom', message: 'At most one primary metric' });
          break;
        case 'time-series':
          unique(component.datasets, 'Duplicate dataset');
          for (const data of component.datasets) {
            unique(data.series, 'Duplicate series');
            for (let index = 0; index < data.points.length; index++) {
              const point = data.points[index];
              if (point.values.length !== data.series.length) ctx.addIssue({ code: 'custom', message: 'Point width must match series' });
              if (index > 0 && point.date <= data.points[index - 1].date) ctx.addIssue({ code: 'custom', message: 'Dates must increase' });
              if (data.style === 'stacked' && point.values.some(value => value === null || value < 0)) ctx.addIssue({ code: 'custom', message: 'Stacked data must be complete and nonnegative' });
            }
          }
          break;
        case 'distribution': unique(component.slices, 'Duplicate slice'); break;
        case 'table':
          if (!inlineTableBlockPayloadSchema.safeParse(component.table).success) ctx.addIssue({ code: 'custom', message: 'Invalid inline table' });
          break;
        case 'records':
          unique(component.datasets, 'Duplicate dataset');
          for (const data of component.datasets) {
            unique(data.items, 'Duplicate record');
            for (const item of data.items) unique(item.disclosures, 'Duplicate disclosure');
          }
          break;
        case 'meter':
          if (component.limit !== null && component.limit <= 0) ctx.addIssue({ code: 'custom', message: 'Meter limit must be positive' });
          break;
        case 'bars': break;
      }
    }
  }
}

export const nativeViewPayloadSchema: z.ZodType<NativeViewPayload> = z.unknown()
  .superRefine(readBudget).superRefine(rejectReservedObjectKeys)
  .pipe(nativeViewShapeSchema as z.ZodType<NativeViewPayload>).superRefine(relations);
